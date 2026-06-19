//! The deterministic orchestrator. Pure code — no LLM decides routing or FSM
//! transitions. It drives one user turn across the swarm:
//!
//! 1. (sticky-facts) MemoryAgent extracts facts before the responder.
//! 2. (scoped) TopicAgent routes; may short-circuit with a canned answer.
//!    Task switching: a new-task intent parks the current task (paused, kept in
//!    the backlog) and opens a fresh Clarify task — the only way to leave the
//!    terminal Done stage; a "back to …" intent resumes a paused task.
//! 3. Pick the responder deterministically by current FSM stage.
//! 4. InvariantAgent validates → retry the responder on violations (bounded).
//!    If violations survive the retries, the answer is BLOCKED and replaced with
//!    a refusal — invariants outrank the request, never shipped when broken.
//! 5. Commit the answer; advance the FSM in code via `allowed_next`.
//! 6. Post-turn service agents: Memory (non-sticky), Summary, Profile.

use crate::chat::agent::{
    StageTransition, StatefulReport, build_invariant_refusal_for_prompt,
    build_invariant_unverified_response, is_invariant_compliance_response, local_chat_response,
    merge_optional_metrics, paused_user_supplied_next_info, requested_task_stage,
    sanitize_unverified_legal_claims, starts_new_task, switch_back_target,
    task_decision_from_exchange, task_resume_hint, task_title_from_prompt,
};
use crate::chat::memory::MemoryStrategy;
use crate::chat::store::{TaskArtifact, TaskPauseReason, TaskStage};
use crate::errors::AppError;
use crate::providers::{ChatMessage, ChatRequest, ChatResponse, RequestMetrics, Role};

use super::agent::{InvariantCheckStatus, SubAgent, SwarmTurn};
use super::agents::{
    GeneralAgent, InvariantAgent, MemoryAgent, PipelineWorkerAgent, ProfileAgent, StageAgent,
    SummaryAgent, TopicAgent, lifecycle_safe_control, strip_stage_marker, update_active_topic_file,
};
use super::config::SubAgentRole;
use super::prompt_builder::PromptBuilder;

/// Max responder retries when the Invariant agent reports violations.
const MAX_INVARIANT_RETRIES: usize = 2;

/// Owns the agent entities and drives a turn deterministically.
pub struct SwarmOrchestrator {
    planning: StageAgent,
    execution: StageAgent,
    validation: StageAgent,
    done: StageAgent,
    general: GeneralAgent,
    memory: MemoryAgent,
    summary: SummaryAgent,
    profile: ProfileAgent,
    invariant: InvariantAgent,
    topic: TopicAgent,
}

impl Default for SwarmOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmOrchestrator {
    pub fn new() -> Self {
        Self {
            planning: StageAgent::new(SubAgentRole::Planning),
            execution: StageAgent::new(SubAgentRole::Execution),
            validation: StageAgent::new(SubAgentRole::Validation),
            done: StageAgent::new(SubAgentRole::Done),
            general: GeneralAgent,
            memory: MemoryAgent,
            summary: SummaryAgent,
            profile: ProfileAgent,
            invariant: InvariantAgent,
            topic: TopicAgent,
        }
    }

    /// Deterministic responder for a task stage. Clarify folds into Planning.
    fn responder(&self, role: SubAgentRole) -> &dyn SubAgent {
        match role {
            SubAgentRole::Planning => &self.planning,
            SubAgentRole::Execution => &self.execution,
            SubAgentRole::Validation => &self.validation,
            SubAgentRole::Done => &self.done,
            _ => &self.general,
        }
    }

    /// Run one full user turn. Returns the user-facing response and the stateful
    /// report (stage, transition, violations, pending questions).
    pub async fn run_turn(
        &self,
        turn: &mut SwarmTurn<'_>,
    ) -> Result<(ChatResponse, StatefulReport), AppError> {
        let prompt = turn.prompt.to_string();

        // Lifecycle control is evaluated before any provider-backed service
        // agent. Pause/resume/approval therefore remains a local, durable action.
        apply_task_switching(turn, &prompt);
        if let Some((message, transition)) = apply_lifecycle_control(turn, &prompt) {
            return Ok(self.local_lifecycle_result(turn, &prompt, message, transition));
        }
        if let Some((requested, reason)) = lifecycle_request_rejection(turn, &prompt) {
            let current = turn
                .task
                .as_ref()
                .map(|task| task.stage)
                .unwrap_or_default();
            if let Some(task) = turn.task.as_mut() {
                task.transition_block_reason = reason.clone();
            }
            return Ok(self.local_lifecycle_result(
                turn,
                &prompt,
                lifecycle_block_message(current, &reason),
                Some(StageTransition {
                    from: current,
                    to: requested,
                    accepted: false,
                    reason,
                }),
            ));
        }

        // 1. Sticky-facts: extract before the responder so the facts block is fresh.
        if turn.memory_config.strategy == MemoryStrategy::StickyFacts {
            let outcome = self.memory.run(turn).await;
            accumulate(turn, outcome.metrics);
        }

        // 2. Scoped topic routing; may short-circuit with a canned answer.
        if turn.memory_config.strategy == MemoryStrategy::ScopedBranches {
            let outcome = self.topic.run(turn).await;
            accumulate(turn, outcome.metrics);
            if outcome.short_circuit {
                let message = outcome.answer.unwrap_or_default();
                self.commit(turn, &prompt, &message);
                let response = local_chat_response(message, turn.aux_metrics.clone());
                return Ok((response, StatefulReport::default()));
            }
        }

        if let Err(reason) = run_pipeline_workers(turn).await {
            let current = turn
                .task
                .as_ref()
                .map(|task| task.stage)
                .unwrap_or_default();
            if let Some(task) = turn.task.as_mut() {
                task.transition_block_reason = reason.clone();
            }
            return Ok(self.local_lifecycle_result(
                turn,
                &prompt,
                lifecycle_block_message(current, &reason),
                None,
            ));
        }

        // 3. Pick responder strictly by the persisted current FSM stage. User
        // intent may be rejected above, but can never select a future agent.
        let responder_role = match turn.task.as_ref() {
            Some(task) => stage_responder_role(task.stage),
            None => SubAgentRole::General,
        };
        turn.retry_violations.clear();
        let mut outcome = self.responder(responder_role).run(turn).await;
        let mut main_metrics = outcome.metrics.clone();
        turn.pending_answer = outcome.answer.clone();

        // 4. Invariant validation + bounded retry.
        let mut violations = Vec::new();
        let mut invariant_status = InvariantCheckStatus::NotRun;
        if !turn.invariants.is_empty() {
            let mut retries = 0;
            loop {
                let inv = self.invariant.run(turn).await;
                accumulate(turn, inv.metrics);
                violations = inv.violations.clone();
                invariant_status = inv.invariant_status;
                if invariant_status != InvariantCheckStatus::Failed
                    || retries >= MAX_INVARIANT_RETRIES
                {
                    break;
                }
                retries += 1;
                turn.retry_violations = violations.clone();
                outcome = self.responder(responder_role).run(turn).await;
                main_metrics = merge_optional_metrics(main_metrics, outcome.metrics.clone());
                turn.pending_answer = outcome.answer.clone();
            }
        }
        turn.retry_violations.clear();

        // 5. Commit answer + deterministic task progress + pause/resume + FSM advance.
        let known_user_context = user_evidence_context(turn, &prompt);
        let mut answer = sanitize_unverified_legal_claims(
            turn.invariants,
            &turn.pending_answer.take().unwrap_or_default(),
            &known_user_context,
        );
        violations = local_invariant_violations_after_sanitize(turn, &answer, violations);
        if violations.is_empty() && invariant_status == InvariantCheckStatus::Failed {
            invariant_status = InvariantCheckStatus::Passed;
        }
        // Hard enforcement: unresolved semantic validation and surviving
        // violations are both blocked. The pending answer is never committed.
        let blocked = if invariant_status == InvariantCheckStatus::Unavailable {
            answer = build_invariant_unverified_response(turn.invariants);
            true
        } else if !violations.is_empty() {
            invariant_status = InvariantCheckStatus::Failed;
            answer = build_invariant_refusal_for_prompt(&violations, &prompt);
            true
        } else {
            if !turn.invariants.is_empty() {
                invariant_status = InvariantCheckStatus::Passed;
            }
            false
        };
        let compliance_only = is_invariant_compliance_response(&answer);
        let preserve_task_state = blocked || compliance_only;
        self.commit(turn, &prompt, &answer);
        if !preserve_task_state {
            seed_task_progress(turn, &prompt, &answer);
        }
        let transition = (!preserve_task_state)
            .then(|| apply_stage_completion(turn, outcome.stage_complete, outcome.artifact.clone()))
            .flatten();
        if let Some(task) = turn.task.as_mut() {
            task.violations = violations.clone();
        }

        // 6. Post-turn service agents.
        if turn.memory_config.strategy != MemoryStrategy::StickyFacts {
            let outcome = self.memory.run(turn).await;
            accumulate(turn, outcome.metrics);
        }
        let summary_outcome = self.summary.run(turn).await;
        accumulate(turn, summary_outcome.metrics);

        let mut stateful = StatefulReport::default();
        if turn.agent_profile.is_some() {
            let outcome = self.profile.run(turn).await;
            accumulate(turn, outcome.metrics);
            if let Some(profile) = turn.agent_profile.as_ref() {
                stateful.pending_questions = profile
                    .pending_required()
                    .into_iter()
                    .map(|field| {
                        let question = field.question.trim();
                        if question.is_empty() {
                            field.key.clone()
                        } else {
                            question.to_string()
                        }
                    })
                    .collect();
            }
        }

        let _ = update_active_topic_file(turn, &prompt, &answer);
        turn.memory
            .apply_scoped_branch_storage_policy(turn.history, turn.memory_config);

        // Surface stateful info for the web debug view.
        if let Some(task) = turn.task.as_ref() {
            stateful.stage = Some(task.stage);
            stateful.current_step = task.current_step.clone();
            stateful.expected_action = task.expected_action.clone();
            stateful.paused = task.paused;
            stateful.resume_hint = task.resume_hint.clone();
            stateful.transition_block_reason = task.transition_block_reason.clone();
            stateful.next_stage = task.next_stage();
            stateful.next_transition_requirement = task.next_transition_requirement();
            stateful.backlog = backlog_titles(task);
        }
        stateful.stage_transition = transition;
        stateful.violations = violations;
        stateful.invariant_status = invariant_status_label(invariant_status).to_string();
        stateful.invariant_summary =
            invariant_summary(invariant_status, turn.invariants, &stateful.violations);
        stateful.metrics = turn.aux_metrics.clone();

        let metrics = combine_response_metrics(main_metrics, &turn.aux_metrics);
        let response = ChatResponse {
            text: answer,
            finish_reason: Some("stop".to_string()),
            metrics,
        };
        Ok((response, stateful))
    }

    /// Prepare a streaming turn: run pre-step service agents (sticky memory,
    /// topic) and build the responder request. Streaming does not use the
    /// stage-done marker (no FSM auto-advance), so the marker never leaks to the
    /// user; stages advance on non-streamed turns.
    pub async fn stream_prepare(&self, turn: &mut SwarmTurn<'_>) -> Result<StreamPrep, AppError> {
        let prompt = turn.prompt.to_string();
        apply_task_switching(turn, &prompt);
        if let Some((message, transition)) = apply_lifecycle_control(turn, &prompt) {
            let (response, report) =
                self.local_lifecycle_result(turn, &prompt, message, transition);
            return Ok(StreamPrep::Local(
                response,
                turn.aux_metrics.clone(),
                report,
            ));
        }
        if let Some((requested, reason)) = lifecycle_request_rejection(turn, &prompt) {
            let current = turn
                .task
                .as_ref()
                .map(|task| task.stage)
                .unwrap_or_default();
            if let Some(task) = turn.task.as_mut() {
                task.transition_block_reason = reason.clone();
            }
            let (response, report) = self.local_lifecycle_result(
                turn,
                &prompt,
                lifecycle_block_message(current, &reason),
                Some(StageTransition {
                    from: current,
                    to: requested,
                    accepted: false,
                    reason,
                }),
            );
            return Ok(StreamPrep::Local(
                response,
                turn.aux_metrics.clone(),
                report,
            ));
        }
        if turn.memory_config.strategy == MemoryStrategy::StickyFacts {
            let outcome = self.memory.run(turn).await;
            accumulate(turn, outcome.metrics);
        }
        if turn.memory_config.strategy == MemoryStrategy::ScopedBranches {
            let outcome = self.topic.run(turn).await;
            accumulate(turn, outcome.metrics);
            if outcome.short_circuit {
                let message = outcome.answer.unwrap_or_default();
                self.commit(turn, &prompt, &message);
                let response = local_chat_response(message, turn.aux_metrics.clone());
                return Ok(StreamPrep::Local(
                    response,
                    turn.aux_metrics.clone(),
                    StatefulReport::default(),
                ));
            }
        }
        if let Err(reason) = run_pipeline_workers(turn).await {
            let current = turn
                .task
                .as_ref()
                .map(|task| task.stage)
                .unwrap_or_default();
            if let Some(task) = turn.task.as_mut() {
                task.transition_block_reason = reason.clone();
            }
            let (response, report) = self.local_lifecycle_result(
                turn,
                &prompt,
                lifecycle_block_message(current, &reason),
                None,
            );
            return Ok(StreamPrep::Local(
                response,
                turn.aux_metrics.clone(),
                report,
            ));
        }
        let role = match turn.task.as_ref() {
            Some(task) => stage_responder_role(task.stage),
            None => SubAgentRole::General,
        };
        let (profile, _token) = turn.responder_profile(role);
        // Same stage rules as a non-streamed turn (with the completion marker) so
        // the FSM advances on streamed turns too; the web layer filters the
        // marker out of the live token stream.
        let messages = PromptBuilder::build(turn, super::agents::stage_rules(role));
        let request = ChatRequest {
            model: profile.model.clone(),
            messages,
            control: lifecycle_safe_control(turn.control, role),
            pricing: turn.pricing.clone(),
            billing: turn.billing.clone(),
        };
        Ok(StreamPrep::Request(request, turn.aux_metrics.clone()))
    }

    /// Finalize a streaming turn after the answer streamed: commit, run post
    /// service agents, validate invariants (no retry while streaming), persist
    /// topic file, and produce the stateful report. Returns auxiliary metrics.
    pub async fn stream_finalize(
        &self,
        turn: &mut SwarmTurn<'_>,
        prompt: &str,
        raw_answer: &str,
    ) -> (StatefulReport, Option<RequestMetrics>, String) {
        // Parse the stage marker, commit the cleaned answer, then advance the FSM
        // deterministically — same path as a non-streamed turn.
        let (raw_answer, complete, key) = strip_stage_marker(raw_answer);
        let known_user_context = user_evidence_context(turn, prompt);
        let mut answer =
            sanitize_unverified_legal_claims(turn.invariants, &raw_answer, &known_user_context);

        // Invariant validation BEFORE commit (streaming does not retry). If the
        // answer still breaks an invariant, block it and commit a refusal instead
        // — the same hard enforcement as the non-streamed turn.
        let mut violations = Vec::new();
        let mut invariant_status = InvariantCheckStatus::NotRun;
        if !turn.invariants.is_empty() {
            turn.pending_answer = Some(answer.clone());
            let inv = self.invariant.run(turn).await;
            accumulate(turn, inv.metrics);
            turn.pending_answer = None;
            invariant_status = inv.invariant_status;
            violations = local_invariant_violations_after_sanitize(turn, &answer, inv.violations);
        }

        if violations.is_empty() && invariant_status == InvariantCheckStatus::Failed {
            invariant_status = InvariantCheckStatus::Passed;
        }
        let blocked = if invariant_status == InvariantCheckStatus::Unavailable {
            answer = build_invariant_unverified_response(turn.invariants);
            true
        } else if !violations.is_empty() {
            invariant_status = InvariantCheckStatus::Failed;
            answer = build_invariant_refusal_for_prompt(&violations, prompt);
            true
        } else {
            if !turn.invariants.is_empty() {
                invariant_status = InvariantCheckStatus::Passed;
            }
            false
        };
        let compliance_only = is_invariant_compliance_response(&answer);
        let preserve_task_state = blocked || compliance_only;
        self.commit(turn, prompt, &answer);
        if !preserve_task_state {
            seed_task_progress(turn, prompt, &answer);
        }
        let transition = if !preserve_task_state {
            apply_stage_completion(
                turn,
                complete,
                complete.then(|| (key, truncate(&answer, 280))),
            )
        } else {
            None
        };
        if turn.memory_config.strategy != MemoryStrategy::StickyFacts {
            let outcome = self.memory.run(turn).await;
            accumulate(turn, outcome.metrics);
        }
        let summary_outcome = self.summary.run(turn).await;
        accumulate(turn, summary_outcome.metrics);

        let mut stateful = StatefulReport::default();
        if turn.agent_profile.is_some() {
            let outcome = self.profile.run(turn).await;
            accumulate(turn, outcome.metrics);
            if let Some(profile) = turn.agent_profile.as_ref() {
                stateful.pending_questions = profile
                    .pending_required()
                    .into_iter()
                    .map(|field| {
                        let question = field.question.trim();
                        if question.is_empty() {
                            field.key.clone()
                        } else {
                            question.to_string()
                        }
                    })
                    .collect();
            }
        }

        if let Some(task) = turn.task.as_mut() {
            task.violations = violations.clone();
        }

        let _ = update_active_topic_file(turn, prompt, &answer);
        turn.memory
            .apply_scoped_branch_storage_policy(turn.history, turn.memory_config);

        if let Some(task) = turn.task.as_ref() {
            stateful.stage = Some(task.stage);
            stateful.current_step = task.current_step.clone();
            stateful.expected_action = task.expected_action.clone();
            stateful.paused = task.paused;
            stateful.resume_hint = task.resume_hint.clone();
            stateful.transition_block_reason = task.transition_block_reason.clone();
            stateful.next_stage = task.next_stage();
            stateful.next_transition_requirement = task.next_transition_requirement();
            stateful.backlog = backlog_titles(task);
        }
        stateful.stage_transition = transition;
        stateful.violations = violations;
        stateful.invariant_status = invariant_status_label(invariant_status).to_string();
        stateful.invariant_summary =
            invariant_summary(invariant_status, turn.invariants, &stateful.violations);
        let aux = turn.aux_metrics.clone();
        stateful.metrics = aux.clone();
        (stateful, aux, answer)
    }

    fn local_lifecycle_result(
        &self,
        turn: &mut SwarmTurn<'_>,
        prompt: &str,
        message: String,
        transition: Option<StageTransition>,
    ) -> (ChatResponse, StatefulReport) {
        self.commit(turn, prompt, &message);
        let mut stateful = StatefulReport {
            stage_transition: transition,
            invariant_status: "not_run".to_string(),
            invariant_summary: "Локальный lifecycle guard; ответ модели не запускался.".to_string(),
            metrics: turn.aux_metrics.clone(),
            ..StatefulReport::default()
        };
        if let Some(task) = turn.task.as_ref() {
            stateful.stage = Some(task.stage);
            stateful.current_step = task.current_step.clone();
            stateful.expected_action = task.expected_action.clone();
            stateful.paused = task.paused;
            stateful.resume_hint = task.resume_hint.clone();
            stateful.transition_block_reason = task.transition_block_reason.clone();
            stateful.next_stage = task.next_stage();
            stateful.next_transition_requirement = task.next_transition_requirement();
            stateful.backlog = backlog_titles(task);
        }
        (
            local_chat_response(message, turn.aux_metrics.clone()),
            stateful,
        )
    }

    /// Append the user prompt + answer to history and record branch labels.
    fn commit(&self, turn: &mut SwarmTurn<'_>, prompt: &str, answer: &str) {
        let user_index = turn.history.len();
        turn.history.push(ChatMessage {
            role: Role::User,
            content: prompt.to_string(),
        });
        let assistant_index = turn.history.len();
        turn.history.push(ChatMessage {
            role: Role::Assistant,
            content: answer.to_string(),
        });
        turn.memory
            .record_turn_branch(user_index, assistant_index, turn.memory_config);
    }
}

fn local_invariant_violations_after_sanitize(
    turn: &SwarmTurn<'_>,
    answer: &str,
    previous: Vec<String>,
) -> Vec<String> {
    let mut violations = previous;
    let legal_invariant = turn.invariants.iter().find(|invariant| {
        let normalized = invariant.to_lowercase();
        normalized.contains("не выдумывать") && normalized.contains("норм")
    });
    if let Some(legal_invariant) = legal_invariant {
        let remaining = crate::chat::agent::local_invariant_violations(
            std::slice::from_ref(legal_invariant),
            answer,
        );
        if remaining.is_empty() {
            violations.retain(|violation| violation != legal_invariant);
        }
    }
    violations
}

fn user_evidence_context(turn: &SwarmTurn<'_>, prompt: &str) -> String {
    let mut context = turn
        .history
        .iter()
        .filter(|message| message.role == Role::User)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    context.push('\n');
    context.push_str(prompt);
    context
}

/// Result of preparing a streaming turn.
pub enum StreamPrep {
    /// Stream this request from the resolved responder.
    Request(ChatRequest, Option<RequestMetrics>),
    /// A terminal local answer (e.g. topic routing could not match).
    Local(ChatResponse, Option<RequestMetrics>, StatefulReport),
}

/// Map a task stage to its responder role (Clarify folds into Planning).
fn stage_responder_role(stage: TaskStage) -> SubAgentRole {
    match stage {
        TaskStage::Clarify | TaskStage::Planning => SubAgentRole::Planning,
        TaskStage::Execution => SubAgentRole::Execution,
        TaskStage::Validation => SubAgentRole::Validation,
        TaskStage::Done => SubAgentRole::Done,
    }
}

async fn run_pipeline_workers(turn: &mut SwarmTurn<'_>) -> Result<(), String> {
    let Some(task) = turn.task.as_ref() else {
        return Ok(());
    };
    if task.paused {
        return Ok(());
    }
    let stage = task.stage;
    let workers = task
        .active_pipeline_stage()
        .map(|pipeline| pipeline.worker_agents.clone())
        .unwrap_or_default();
    for worker in workers {
        if worker.id.trim().is_empty() {
            return Err(format!(
                "Pipeline worker для стадии {stage} не имеет обязательного id."
            ));
        }
        let outcome = PipelineWorkerAgent::new(worker.clone(), stage)
            .run(turn)
            .await;
        accumulate(turn, outcome.metrics);
        let Some((key, value)) = outcome.artifact else {
            return Err(format!(
                "Pipeline worker `{}` не создал результат; стадия {stage} остановлена.",
                worker.id.trim()
            ));
        };
        let artifact = TaskArtifact { stage, key, value };
        turn.worker_outputs.push(artifact.clone());
        if let Some(task) = turn.task.as_mut() {
            task.artifacts.push(artifact);
        }
    }
    Ok(())
}

/// Deterministic multi-task switching (no LLM): a clear new-task intent parks the
/// active task — paused and preserved in the backlog — and starts a fresh
/// `Clarify` task; a "back to …" intent resumes a matching paused task. This is
/// the only path that lets the FSM leave the terminal `Done` stage: a finished
/// task cannot advance, so a new request opens a new task instead.
fn apply_task_switching(turn: &mut SwarmTurn<'_>, prompt: &str) {
    let Some(task) = turn.task.as_mut() else {
        return;
    };
    // Resuming a paused task takes precedence over starting a brand-new one.
    if let Some(index) = switch_back_target(prompt, &task.backlog) {
        task.switch_to_backlog(index);
        return;
    }
    if starts_new_task(task.stage, prompt) {
        let hint = task_resume_hint(task, prompt, "");
        task.start_new_task(prompt.trim(), hint);
    }
}

/// Deterministic stage completion: pipeline-contract advance (with human-approval
/// pause) when the task has a pipeline, else plain `allowed_next` advance. No LLM
/// decides the target stage.
fn apply_stage_completion(
    turn: &mut SwarmTurn<'_>,
    complete: bool,
    artifact: Option<(String, String)>,
) -> Option<StageTransition> {
    if !complete {
        return None;
    }
    let task = turn.task.as_mut()?;
    if task.paused {
        return None;
    }
    let current = task.stage;
    let (key, value) = artifact.unwrap_or_else(|| {
        (
            current.to_string(),
            "Stage responder returned no artifact.".to_string(),
        )
    });
    if value.trim().is_empty() {
        let reason = format!("Стадия {current} не завершена: результат пуст.");
        task.transition_block_reason = reason.clone();
        return Some(StageTransition {
            from: current,
            to: current,
            accepted: false,
            reason,
        });
    }
    if !task.pipeline.is_empty() {
        if let Err(reason) = task.validate_pipeline() {
            task.transition_block_reason = reason.clone();
            return Some(StageTransition {
                from: current,
                to: current,
                accepted: false,
                reason,
            });
        }
        if current == TaskStage::Planning && task.plan.is_empty() {
            task.plan = plan_lines(&value);
        }
        let advance = task.complete_pipeline_stage(key, value)?;
        return Some(StageTransition {
            from: advance.from,
            to: advance.to,
            accepted: advance.accepted,
            reason: task.transition_block_reason.clone(),
        });
    }
    if !task.record_stage_artifact(current, key, value.clone()) {
        return Some(StageTransition {
            from: current,
            to: current,
            accepted: false,
            reason: task.transition_block_reason.clone(),
        });
    }
    if current == TaskStage::Planning {
        if task.plan.is_empty() {
            task.plan = plan_lines(&value);
        }
        task.pause_for(
            TaskPauseReason::PlanApproval,
            "Проверьте подготовленный план и явно напишите «утверждаю план».",
        );
        task.current_step = "утвердить план".to_string();
        task.expected_action = "approve_plan".to_string();
        return Some(StageTransition {
            from: current,
            to: current,
            accepted: true,
            reason: "План подготовлен; переход в execution ожидает явного approval.".to_string(),
        });
    }
    if current == TaskStage::Validation {
        task.validation_passed = true;
    }
    let next = current.canonical_next()?;
    let decision = task.try_transition(next);
    Some(StageTransition {
        from: decision.from,
        to: decision.to,
        accepted: decision.accepted,
        reason: decision.reason,
    })
}

/// Apply pause/resume/approval before a provider call. Approval uses explicit
/// whole-message intent; substring matches such as `ок` inside `срок` are never
/// accepted.
fn apply_lifecycle_control(
    turn: &mut SwarmTurn<'_>,
    user_prompt: &str,
) -> Option<(String, Option<StageTransition>)> {
    let task = turn.task.as_mut()?;
    let prompt = normalize_control_intent(user_prompt);
    if task.paused {
        if matches!(
            task.pause_reason,
            TaskPauseReason::PlanApproval | TaskPauseReason::StageApproval
        ) {
            if !explicit_approval_intent(&prompt, task.pause_reason) {
                let reason = match task.pause_reason {
                    TaskPauseReason::PlanApproval => {
                        "План ожидает явного утверждения. Напишите «утверждаю план»."
                    }
                    TaskPauseReason::StageApproval => {
                        "Артефакт ожидает явного утверждения. Напишите «утверждаю результат»."
                    }
                    _ => unreachable!(),
                };
                task.transition_block_reason = reason.to_string();
                return Some((
                    format!("⏸ {reason}\n\nТекущая стадия: `{}`.", task.stage),
                    None,
                ));
            }
            let from = task.stage;
            if task.approve_pipeline_pause() {
                let to = task.stage;
                return Some((
                    format!(
                        "✅ Approval принят. Задача перешла `{from}` → `{to}`. Следующий ход выполнит только агент стадии `{to}`."
                    ),
                    Some(StageTransition {
                        from,
                        to,
                        accepted: true,
                        reason: "Явное пользовательское approval.".to_string(),
                    }),
                ));
            }
            let reason = task.transition_block_reason.clone();
            return Some((
                lifecycle_block_message(from, &reason),
                Some(StageTransition {
                    from,
                    to: from.canonical_next().unwrap_or(from),
                    accepted: false,
                    reason,
                }),
            ));
        }
        if resume_intent(&prompt) || paused_user_supplied_next_info(&prompt) {
            task.resume();
            return Some((
                format!(
                    "▶️ Задача возобновлена на стадии `{}`. Продолжаю с сохранённого шага: {}.",
                    task.stage,
                    task.resume_hint.trim()
                ),
                None,
            ));
        }
        let reason = "Задача на паузе. Напишите «продолжай», чтобы возобновить её.";
        task.transition_block_reason = reason.to_string();
        return Some((format!("⏸ {reason}"), None));
    }
    if pause_intent(&prompt) {
        let hint = task_resume_hint(task, user_prompt, "");
        task.pause(hint.clone());
        return Some((
            format!(
                "⏸ Задача поставлена на паузу на стадии `{}`. Для продолжения: {}.",
                task.stage, hint
            ),
            None,
        ));
    }
    None
}

fn lifecycle_request_rejection(turn: &SwarmTurn<'_>, prompt: &str) -> Option<(TaskStage, String)> {
    let task = turn.task.as_ref()?;
    let requested = requested_task_stage(prompt)?;
    if requested == task.stage
        || (task.stage == TaskStage::Clarify && requested == TaskStage::Planning)
    {
        return None;
    }
    if requested == TaskStage::Planning
        && matches!(task.stage, TaskStage::Execution | TaskStage::Validation)
    {
        return None;
    }
    let current_index = TaskStage::ORDERED
        .iter()
        .position(|stage| *stage == task.stage)?;
    let requested_index = TaskStage::ORDERED
        .iter()
        .position(|stage| *stage == requested)?;
    if requested_index <= current_index {
        return None;
    }
    let reason = match task.stage {
        TaskStage::Clarify => {
            "Сначала завершите уточнение задачи; реализация и проверка пока заблокированы."
                .to_string()
        }
        TaskStage::Planning => {
            if !task.has_stage_artifact(TaskStage::Planning) {
                "Сначала PlanningAgent должен подготовить план.".to_string()
            } else {
                "Сначала пользователь должен явно утвердить план.".to_string()
            }
        }
        TaskStage::Execution => {
            "Сначала ExecutionAgent должен завершить результат, затем его проверит ValidationAgent."
                .to_string()
        }
        TaskStage::Validation => {
            "Финал доступен только после успешного результата ValidationAgent.".to_string()
        }
        TaskStage::Done => "Задача уже завершена.".to_string(),
    };
    Some((requested, reason))
}

fn lifecycle_block_message(stage: TaskStage, reason: &str) -> String {
    format!(
        "⛔ Нельзя перейти дальше со стадии `{stage}`.\n\n{reason}\n\nСледующее допустимое действие задаётся lifecycle-панелью задачи."
    )
}

fn normalize_control_intent(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn explicit_approval_intent(prompt: &str, reason: TaskPauseReason) -> bool {
    let exact_short = matches!(prompt, "ок" | "ok" | "approved" | "утверждаю" | "одобряю");
    exact_short
        || match reason {
            TaskPauseReason::PlanApproval => [
                "утверждаю план",
                "одобряю план",
                "план утвержден",
                "план утверждён",
                "approve plan",
                "plan approved",
            ]
            .contains(&prompt),
            TaskPauseReason::StageApproval => [
                "утверждаю результат",
                "одобряю результат",
                "результат утвержден",
                "результат утверждён",
                "approve result",
                "artifact approved",
            ]
            .contains(&prompt),
            _ => false,
        }
}

fn resume_intent(prompt: &str) -> bool {
    matches!(
        prompt,
        "continue"
            | "continue task"
            | "resume"
            | "resume task"
            | "продолжай"
            | "продолжить"
            | "продолжить задачу"
            | "снять с паузы"
            | "возобновить"
            | "возобновляй"
    )
}

fn pause_intent(prompt: &str) -> bool {
    matches!(
        prompt,
        "pause"
            | "pause task"
            | "пауза"
            | "поставь на паузу"
            | "поставь задачу на паузу"
            | "приостанови"
            | "приостанови задачу"
            | "останови задачу"
            | "вернусь позже"
            | "потом продолжим"
    )
}

fn plan_lines(answer: &str) -> Vec<String> {
    answer
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == ')')
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .take(12)
        .collect()
}

/// Deterministically seed the task's goal/title/decisions from the exchange
/// (ported from the legacy `capture_task_progress`). No LLM call.
fn seed_task_progress(turn: &mut SwarmTurn<'_>, prompt: &str, answer: &str) {
    let Some(task) = turn.task.as_mut() else {
        return;
    };
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return;
    }
    if task.goal.trim().is_empty() {
        task.goal = prompt.to_string();
    }
    if task.title.trim().is_empty() {
        task.title = task_title_from_prompt(prompt);
    }
    if task.current_step.trim().is_empty() || task.expected_action.trim().is_empty() {
        task.sync_progress_for_stage();
    }
    capture_task_facts(task, prompt);
    if let Some(decision) = task_decision_from_exchange(prompt, answer)
        && !task.results.iter().any(|item| item == &decision)
    {
        task.results.push(decision);
    }
    if let Some(profile) = turn.agent_profile.as_mut() {
        capture_profile_fields(profile, prompt);
    }
}

fn capture_task_facts(task: &mut crate::chat::store::TaskContext, prompt: &str) {
    for (key, value) in extract_legal_task_facts(prompt) {
        let fact = format!("{key}: {value}");
        if !task.results.iter().any(|item| item == &fact) {
            task.results.push(fact);
        }
    }
}

fn capture_profile_fields(profile: &mut crate::chat::store::AgentProfile, prompt: &str) {
    let facts = extract_legal_task_facts(prompt);
    let mut changed = false;
    for field in &mut profile.fields {
        if !field.value.trim().is_empty() {
            continue;
        }
        if let Some((_, value)) = facts.iter().find(|(key, _)| key == &field.key) {
            field.value = value.clone();
            changed = true;
        }
    }
    if changed {
        profile.updated_at_unix = crate::chat::unix_now();
    }
}

fn extract_legal_task_facts(prompt: &str) -> Vec<(String, String)> {
    let lower = prompt.to_lowercase();
    let mut facts = Vec::new();
    if lower.contains("заказчик ооо") || lower.contains("должник ооо") {
        facts.push(("debtor_type".to_string(), "ООО".to_string()));
    }
    if let Some(value) = sentence_with(prompt, &["номер", "договор №", "договор номер"])
    {
        facts.push(("contract_details".to_string(), value));
    }
    if let Some(value) = sentence_with(prompt, &["акт подпис"]) {
        facts.push(("acceptance_act".to_string(), value));
    }
    if let Some(value) = sentence_with(prompt, &["оплата в течение", "срок оплаты"])
    {
        facts.push(("payment_terms".to_string(), value));
    }
    if let Some(value) = value_after_label(prompt, "адрес") {
        facts.push(("debtor_address".to_string(), value));
    }
    if lower.contains("без суда") {
        facts.push((
            "desired_outcome".to_string(),
            "начать с досудебной претензии, без суда".to_string(),
        ));
        facts.push((
            "claim_status".to_string(),
            "претензия ещё не отправлена".to_string(),
        ));
    }
    facts
}

fn sentence_with(text: &str, needles: &[&str]) -> Option<String> {
    text.split(['\n', '.', '!', '?'])
        .map(str::trim)
        .find(|part| {
            let lower = part.to_lowercase();
            needles.iter().any(|needle| lower.contains(needle))
        })
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
}

fn value_after_label(text: &str, label: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let start = lower.find(label)? + label.len();
    let value = text[start..]
        .split(['\n', '.', '!', '?'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches([':', '-', '—', ' '])
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Human-readable labels for the paused tasks on the board (title, falling back
/// to goal), for the web debug view.
fn backlog_titles(task: &crate::chat::store::TaskContext) -> Vec<String> {
    task.backlog
        .iter()
        .map(|parked| {
            let title = parked.title.trim();
            if !title.is_empty() {
                title.to_string()
            } else {
                truncate(parked.goal.trim(), 60)
            }
        })
        .filter(|label| !label.is_empty())
        .collect()
}

fn invariant_status_label(status: InvariantCheckStatus) -> &'static str {
    match status {
        InvariantCheckStatus::NotRun => "not_run",
        InvariantCheckStatus::Passed => "pass",
        InvariantCheckStatus::Failed => "blocked",
        InvariantCheckStatus::Unavailable => "unverified",
    }
}

fn invariant_summary(
    status: InvariantCheckStatus,
    invariants: &[String],
    violations: &[String],
) -> String {
    match status {
        InvariantCheckStatus::NotRun => "Инварианты не заданы.".to_string(),
        InvariantCheckStatus::Passed => format!(
            "PASS: кодовый gate допустил ответ после проверки {} инвариант(ов).",
            invariants.len()
        ),
        InvariantCheckStatus::Failed => format!(
            "BLOCKED: ответ не выдан; нарушено {} инвариант(ов).",
            violations.len()
        ),
        InvariantCheckStatus::Unavailable => {
            "UNVERIFIED: семантическую проверку нельзя подтвердить; ответ заблокирован, требуется уточнение или повторная проверка.".to_string()
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

fn accumulate(turn: &mut SwarmTurn<'_>, metrics: Option<RequestMetrics>) {
    turn.aux_metrics = merge_optional_metrics(turn.aux_metrics.take(), metrics);
}

/// Main responder metrics + accumulated auxiliary metrics.
fn combine_response_metrics(
    main: Option<RequestMetrics>,
    aux: &Option<RequestMetrics>,
) -> RequestMetrics {
    let base = main.unwrap_or(RequestMetrics {
        elapsed_ms: 0,
        usage: None,
        cost: None,
    });
    match aux {
        Some(aux) => crate::chat::add_request_metrics(&base, aux),
        None => base,
    }
}
