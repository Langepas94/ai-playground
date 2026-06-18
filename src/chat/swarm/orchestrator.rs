//! The deterministic orchestrator. Pure code — no LLM decides routing or FSM
//! transitions. It drives one user turn across the swarm:
//!
//! 1. (sticky-facts) MemoryAgent extracts facts before the responder.
//! 2. (scoped) TopicAgent routes; may short-circuit with a canned answer.
//! 3. Pick the responder deterministically by current FSM stage.
//! 4. InvariantAgent validates → retry the responder on violations (bounded).
//! 5. Commit the answer; advance the FSM in code via `allowed_next`.
//! 6. Post-turn service agents: Memory (non-sticky), Summary, Profile.

use crate::chat::agent::{
    StageTransition, StatefulReport, infer_task_state_from_exchange, local_chat_response,
    merge_optional_metrics, paused_user_supplied_next_info, sanitize_unverified_legal_claims,
    task_decision_from_exchange, task_resume_hint, task_title_from_prompt,
};
use crate::chat::memory::MemoryStrategy;
use crate::chat::store::{TaskArtifact, TaskStage};
use crate::errors::AppError;
use crate::providers::{ChatMessage, ChatRequest, ChatResponse, RequestMetrics, Role};

use super::agent::{SubAgent, SwarmTurn};
use super::agents::{
    GeneralAgent, InvariantAgent, MemoryAgent, ProfileAgent, StageAgent, SummaryAgent, TopicAgent,
    strip_stage_marker, update_active_topic_file,
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

        // 3. Pick responder deterministically by current FSM stage.
        let responder_role = match turn.task.as_ref() {
            Some(task) => {
                let response_stage = infer_task_state_from_exchange(task.stage, &prompt, "")
                    .map(|inferred| inferred.stage)
                    .unwrap_or(task.stage);
                stage_responder_role(response_stage)
            }
            None => SubAgentRole::General,
        };
        turn.retry_violations.clear();
        let mut outcome = self.responder(responder_role).run(turn).await;
        let mut main_metrics = outcome.metrics.clone();
        turn.pending_answer = outcome.answer.clone();

        // 4. Invariant validation + bounded retry.
        let mut violations = Vec::new();
        if !turn.invariants.is_empty() {
            let mut retries = 0;
            loop {
                let inv = self.invariant.run(turn).await;
                accumulate(turn, inv.metrics);
                violations = inv.violations.clone();
                if violations.is_empty() || retries >= MAX_INVARIANT_RETRIES {
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
        let answer = sanitize_unverified_legal_claims(
            turn.invariants,
            &turn.pending_answer.take().unwrap_or_default(),
            &known_user_context,
        );
        violations = local_invariant_violations_after_sanitize(turn, &answer, violations);
        self.commit(turn, &prompt, &answer);
        seed_task_progress(turn, &prompt, &answer);
        apply_pause_resume(turn, &prompt, &answer);
        // Deterministic progress + stage from user intent takes precedence.
        // The stage marker is only a fallback for neutral prompts such as
        // "дальше"; otherwise a Planning response could advance itself merely
        // because it finished answering a clarifying fact.
        let has_task_intent = turn.task.as_ref().is_some_and(|task| {
            infer_task_state_from_exchange(task.stage, &prompt, &answer).is_some()
        });
        let mut transition = apply_task_inference(turn, &prompt, &answer);
        if !has_task_intent
            && !paused_user_supplied_next_info(&prompt)
            && let Some(explicit) =
                apply_stage_completion(turn, outcome.stage_complete, outcome.artifact.clone())
        {
            transition = Some(explicit);
        }
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
        }
        stateful.stage_transition = transition;
        stateful.violations = violations;
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
                return Ok(StreamPrep::Local(response, turn.aux_metrics.clone()));
            }
        }
        let role = match turn.task.as_ref() {
            Some(task) => {
                let response_stage = infer_task_state_from_exchange(task.stage, &prompt, "")
                    .map(|inferred| inferred.stage)
                    .unwrap_or(task.stage);
                stage_responder_role(response_stage)
            }
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
            control: turn.control.clone(),
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
        let answer =
            sanitize_unverified_legal_claims(turn.invariants, &raw_answer, &known_user_context);
        self.commit(turn, prompt, &answer);
        seed_task_progress(turn, prompt, &answer);
        apply_pause_resume(turn, prompt, &answer);
        let has_task_intent = turn.task.as_ref().is_some_and(|task| {
            infer_task_state_from_exchange(task.stage, prompt, &answer).is_some()
        });
        let mut transition = apply_task_inference(turn, prompt, &answer);
        if !has_task_intent
            && !paused_user_supplied_next_info(prompt)
            && let Some(explicit) = apply_stage_completion(
                turn,
                complete,
                complete.then(|| (key, truncate(&answer, 280))),
            )
        {
            transition = Some(explicit);
        }
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

        let mut violations = Vec::new();
        if !turn.invariants.is_empty() {
            turn.pending_answer = Some(answer.to_string());
            let inv = self.invariant.run(turn).await;
            accumulate(turn, inv.metrics);
            violations = inv.violations.clone();
            turn.pending_answer = None;
            if let Some(task) = turn.task.as_mut() {
                task.violations = violations.clone();
            }
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
        }
        stateful.stage_transition = transition;
        stateful.violations = violations;
        let aux = turn.aux_metrics.clone();
        stateful.metrics = aux.clone();
        (stateful, aux, answer)
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
    Local(ChatResponse, Option<RequestMetrics>),
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

/// Deterministic task-state from the exchange (keyword inference, no LLM): always
/// sets `current_step`/`expected_action`/`resume_hint`, and advances the stage
/// when the user's intent clearly moves it (validated by `can_transition`). This
/// keeps the FSM tracking the actual request instead of lagging a turn behind.
fn apply_task_inference(
    turn: &mut SwarmTurn<'_>,
    prompt: &str,
    answer: &str,
) -> Option<StageTransition> {
    let current = turn.task.as_ref()?.stage;
    if turn.task.as_ref()?.paused {
        return None;
    }
    let inferred = infer_task_state_from_exchange(current, prompt, answer)?;
    let task = turn.task.as_mut()?;
    task.set_progress(inferred.current_step, inferred.expected_action);
    if !inferred.resume_hint.trim().is_empty() {
        task.resume_hint = inferred.resume_hint;
    }
    if inferred.stage != current && current.can_transition(inferred.stage) {
        task.stage = inferred.stage;
        return Some(StageTransition {
            from: current,
            to: inferred.stage,
            accepted: true,
        });
    }
    None
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
    let (key, value) = artifact.unwrap_or_default();
    if !task.pipeline.is_empty() {
        let advance = task.complete_pipeline_stage(key, value)?;
        return Some(StageTransition {
            from: advance.from,
            to: advance.to,
            accepted: advance.accepted,
        });
    }
    if !key.is_empty() || !value.is_empty() {
        task.artifacts.push(TaskArtifact {
            stage: task.stage,
            key,
            value,
        });
    }
    let from = task.stage;
    let &next = from.allowed_next().first()?;
    task.stage = next;
    Some(StageTransition {
        from,
        to: next,
        accepted: true,
    })
}

/// Deterministic pause/resume intent (ported from the legacy
/// `apply_pause_resume_intent`). Keyword-based, no LLM.
fn apply_pause_resume(turn: &mut SwarmTurn<'_>, user_prompt: &str, answer: &str) {
    let Some(task) = turn.task.as_mut() else {
        return;
    };
    let prompt = user_prompt.trim().to_lowercase();
    if task.paused
        && (prompt.contains("resume")
            || prompt.contains("continue")
            || prompt.contains("approve")
            || prompt.contains("approved")
            || prompt.contains("ok")
            || prompt.contains("okay")
            || prompt.contains("продолж")
            || prompt.contains("возобнов")
            || prompt.contains("утвержд")
            || prompt.contains("одобря")
            || prompt.contains("ок")
            || paused_user_supplied_next_info(&prompt))
    {
        if !task.approve_pipeline_pause() {
            task.resume();
        }
        return;
    }
    if !task.paused
        && (prompt.contains("pause")
            || prompt.contains("paused")
            || prompt.contains("пауз")
            || prompt.contains("приостанов")
            || prompt.contains("останов")
            || prompt.contains("вернусь позже")
            || prompt.contains("потом продолжим"))
    {
        let hint = task_resume_hint(task, user_prompt, answer);
        task.pause(hint);
    }
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
    if let Some(decision) = task_decision_from_exchange(prompt, answer)
        && !task.results.iter().any(|item| item == &decision)
    {
        task.results.push(decision);
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
