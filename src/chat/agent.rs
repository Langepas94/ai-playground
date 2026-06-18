use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, RequestMetrics, ResponseControl, Role,
    },
};

use super::memory::{AgentMemory, MemoryConfig, MemoryLayer, MemoryStrategy, TopicRouteDecision};
use super::store::{ProfileField, TaskStage, TopicFileStorage};
use super::swarm::{ResolvedSwarm, SwarmOrchestrator, SwarmReport, SwarmTurn};
use super::token_accounting::{TokenEstimate, estimate_exchange};

pub const LOCAL_SESSION_AGENT_ID: &str = "local-session-agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub history_storage: &'static str,
}

const AGENTS: &[AgentDescriptor] = &[AgentDescriptor {
    id: LOCAL_SESSION_AGENT_ID,
    name: "Локальный чат-агент с историей",
    history_storage: "Локально",
}];

pub fn available_agents() -> &'static [AgentDescriptor] {
    AGENTS
}

pub fn selected_agent(agent_id: Option<&str>) -> Result<&'static AgentDescriptor, AppError> {
    let id = agent_id
        .and_then(|value| {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        })
        .unwrap_or(LOCAL_SESSION_AGENT_ID);
    AGENTS
        .iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| AppError::InvalidInput(format!("Unsupported agent: {id}")))
}

#[derive(Debug)]
pub struct PreparedStreamRequest {
    pub request: Option<ChatRequest>,
    pub context_metrics: Option<RequestMetrics>,
    pub local_response: Option<ChatResponse>,
}

#[derive(Debug, Clone)]
pub struct ChatAgent {
    profile: ProfileConfig,
    token: String,
    history: Vec<ChatMessage>,
    memory: AgentMemory,
    memory_config: MemoryConfig,
    control: ResponseControl,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
    context_limit: Option<u32>,
    topic_store: Option<TopicFileStorage>,
    /// Working-memory layer: the agent's task + stage FSM. Mutated by the
    /// stateful post-processing (stage advance, invariant feedback).
    task_state: Option<super::store::TaskContext>,
    /// Long-term layer: the agent's interviewed profile. Mutated when the agent
    /// elicits a field value from the user. (Distinct from `profile`, which is
    /// the provider/model config.)
    agent_profile: Option<super::store::AgentProfile>,
    /// Hard constraints injected into the prompt and checked against responses.
    invariants: Vec<String>,
    /// Free-text domain description, injected to keep the agent on-domain.
    domain: String,
    /// Reusable user preferences selected at runtime. This is intentionally
    /// separate from the agent definition and never changes tools/capabilities.
    user_profile: Option<super::store::UserProfile>,
    /// Resolved mandatory swarm: 4 stage responders + general + memory/summary/
    /// topic/profile/invariant, each with its own provider/model/prompt.
    swarm: ResolvedSwarm,
    /// Stateful report from the most recent orchestrated turn (stage, transition,
    /// violations, pending questions), surfaced to the web layer.
    last_stateful: StatefulReport,
    /// Swarm activity report from the most recent turn, for the UI swarm panel.
    last_swarm_report: SwarmReport,
}

impl ChatAgent {
    pub fn new(
        profile: ProfileConfig,
        token: String,
        history: Vec<ChatMessage>,
        memory: AgentMemory,
        control: ResponseControl,
        pricing: Option<ModelPricing>,
        billing: Option<BillingLookup>,
    ) -> Self {
        let swarm = ResolvedSwarm::inherit_all(&profile, &token);
        Self {
            profile,
            token,
            history,
            memory,
            memory_config: MemoryConfig::default(),
            control,
            pricing,
            billing,
            context_limit: None,
            topic_store: None,
            task_state: None,
            agent_profile: None,
            invariants: Vec::new(),
            domain: String::new(),
            user_profile: None,
            swarm,
            last_stateful: StatefulReport::default(),
            last_swarm_report: SwarmReport::default(),
        }
    }

    /// Take the stateful report (stage, transition, violations, pending
    /// questions) produced by the most recent orchestrated turn.
    pub fn take_stateful_report(&mut self) -> StatefulReport {
        std::mem::take(&mut self.last_stateful)
    }

    /// Swarm activity report from the most recent turn (per-agent runs).
    pub fn swarm_report(&self) -> &SwarmReport {
        &self.last_swarm_report
    }

    /// Build the per-turn `SwarmTurn` context and run the deterministic
    /// orchestrator. Stores the stateful + swarm reports for the caller and
    /// returns the user-facing response, provider debug and auxiliary metrics.
    async fn orchestrate(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Result<(ChatResponse, ProviderExchangeDebug, Option<RequestMetrics>), AppError> {
        let orchestrator = SwarmOrchestrator::new();
        let (response, stateful, debug, aux, report) = {
            let mut turn = self.build_turn(client, prompt);
            let (response, mut stateful) = orchestrator.run_turn(&mut turn).await?;
            // Auxiliary metrics are already folded into `response.metrics`; null
            // them on the stateful report so the web layer does not double-count.
            stateful.metrics = None;
            let debug = turn
                .captured_debug
                .take()
                .unwrap_or_else(local_provider_debug);
            let aux = turn.aux_metrics.take();
            (response, stateful, debug, aux, turn.report)
        };
        self.last_stateful = stateful;
        self.last_swarm_report = report;
        Ok((response, debug, aux))
    }

    /// Install a resolved mandatory swarm (per-role provider/model/prompt).
    /// Callers build it from a persisted `SwarmConfig` via `resolve_swarm`.
    pub fn set_swarm(&mut self, swarm: ResolvedSwarm) {
        self.swarm = swarm;
    }

    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    pub fn memory(&self) -> &AgentMemory {
        &self.memory
    }

    pub fn set_memory(&mut self, memory: AgentMemory) {
        self.memory = memory;
    }

    pub fn memory_config(&self) -> MemoryConfig {
        self.memory_config.clone()
    }

    pub fn set_memory_config(&mut self, memory_config: MemoryConfig) {
        self.memory_config = memory_config;
    }

    pub fn set_context_limit(&mut self, context_limit: Option<u32>) {
        self.context_limit = context_limit.filter(|limit| *limit > 0);
    }

    pub fn set_topic_store(&mut self, topic_store: Option<TopicFileStorage>) {
        self.topic_store = topic_store;
    }

    /// Working-memory layer: the current task + stage. Injected into each request
    /// as a tagged system block and mutated by stateful post-processing.
    pub fn set_task_state(&mut self, task: Option<super::store::TaskContext>) {
        self.task_state = task;
    }

    /// Read back the (possibly advanced) task state so the web layer can persist it.
    pub fn task_state(&self) -> Option<&super::store::TaskContext> {
        self.task_state.as_ref()
    }

    /// Long-term layer: the agent's profile schema + filled values. Injected and
    /// auto-filled from the dialog.
    pub fn set_agent_profile(&mut self, profile: Option<super::store::AgentProfile>) {
        self.agent_profile = profile;
    }

    /// Read back the (possibly updated) profile so the web layer can persist it.
    pub fn agent_profile(&self) -> Option<&super::store::AgentProfile> {
        self.agent_profile.as_ref()
    }

    /// Hard constraints, injected into the prompt and checked against responses.
    pub fn set_invariants(&mut self, invariants: Vec<String>) {
        self.invariants = invariants
            .into_iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
    }

    pub fn has_invariants(&self) -> bool {
        !self.invariants.is_empty()
    }

    /// Free-text domain description that keeps the agent on-topic.
    pub fn set_domain(&mut self, domain: impl Into<String>) {
        self.domain = domain.into().trim().to_string();
    }

    pub fn set_user_profile(&mut self, profile: Option<super::store::UserProfile>) {
        self.user_profile = profile;
    }

    pub fn context_limit(&self) -> Option<u32> {
        self.context_limit
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.memory = AgentMemory::default();
    }

    pub fn set_control(&mut self, control: ResponseControl) {
        self.control = control;
    }

    /// Record the streamed user prompt + assistant answer into history. The rest
    /// of the turn (service agents, FSM, topic persist) runs in `finalize_stream`.
    pub fn record_stream_response(&mut self, prompt: String, answer: String) {
        self.commit_turn(prompt, answer);
    }

    pub async fn respond(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<ChatResponse, AppError> {
        let (response, _debug, _aux) = self.orchestrate(client, &prompt).await?;
        Ok(response)
    }

    pub async fn respond_with_debug(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let (response, debug, _) = self
            .respond_with_debug_and_context_metrics(client, prompt)
            .await?;
        Ok((response, debug))
    }

    pub async fn respond_with_debug_and_context_metrics(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<
        (
            ChatResponse,
            ProviderExchangeDebug,
            Option<crate::providers::RequestMetrics>,
        ),
        AppError,
    > {
        self.orchestrate(client, &prompt).await
    }

    pub async fn prepare_stream_request(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Result<PreparedStreamRequest, AppError> {
        let orchestrator = SwarmOrchestrator::new();
        let prep = {
            let mut turn = self.build_turn(client, prompt);
            let prep = orchestrator.stream_prepare(&mut turn).await?;
            let report = turn.report;
            (prep, report)
        };
        let (prep, report) = prep;
        self.last_swarm_report = report;
        Ok(match prep {
            super::swarm::orchestrator::StreamPrep::Request(request, context_metrics) => {
                PreparedStreamRequest {
                    request: Some(request),
                    context_metrics,
                    local_response: None,
                }
            }
            super::swarm::orchestrator::StreamPrep::Local(response, context_metrics) => {
                PreparedStreamRequest {
                    request: None,
                    context_metrics,
                    local_response: Some(response),
                }
            }
        })
    }

    /// Finish a streamed turn: strip the stage marker, commit the cleaned answer,
    /// advance the FSM deterministically, run post service agents + invariant
    /// check, persist topic state, store the stateful report. Returns the cleaned
    /// answer (marker removed) plus auxiliary token metrics.
    pub async fn finalize_stream(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
        raw_answer: &str,
    ) -> (String, Option<RequestMetrics>) {
        let orchestrator = SwarmOrchestrator::new();
        let (stateful, aux, clean, report) = {
            let mut turn = self.build_turn(client, prompt);
            let (stateful, aux, clean) = orchestrator
                .stream_finalize(&mut turn, prompt, raw_answer)
                .await;
            (stateful, aux, clean, turn.report)
        };
        self.last_stateful = stateful;
        self.last_swarm_report = report;
        (clean, aux)
    }

    /// Construct a `SwarmTurn` borrowing this agent's fields for one turn.
    fn build_turn<'a>(
        &'a mut self,
        client: &'a dyn ProviderClient,
        prompt: &'a str,
    ) -> SwarmTurn<'a> {
        SwarmTurn {
            client,
            roster: &self.swarm,
            main_profile: &self.profile,
            main_token: &self.token,
            control: &self.control,
            pricing: &self.pricing,
            billing: &self.billing,
            context_limit: self.context_limit,
            topic_store: self.topic_store.as_ref(),
            memory_config: &mut self.memory_config,
            memory: &mut self.memory,
            history: &mut self.history,
            task: &mut self.task_state,
            agent_profile: &mut self.agent_profile,
            user_profile: &self.user_profile,
            invariants: &self.invariants,
            domain: &self.domain,
            prompt,
            pending_answer: None,
            retry_violations: Vec::new(),
            aux_metrics: None,
            captured_debug: None,
            report: SwarmReport::default(),
        }
    }

    pub fn control(&self) -> &ResponseControl {
        &self.control
    }

    pub fn estimate_next_exchange(
        &self,
        prompt: &str,
        response_text: &str,
        context_limit: u32,
    ) -> TokenEstimate {
        let request = self.request_with_user_prompt(prompt.to_string());
        estimate_exchange(
            &request.messages,
            &self.history,
            response_text,
            context_limit,
            self.pricing.as_ref(),
        )
    }

    /// Insert the stateful memory blocks after the leading system messages, so
    /// the provider sees the agent's state up front.
    fn inject_memory_layers(&self, messages: &mut Vec<ChatMessage>) {
        let mut blocks = Vec::new();
        let mut seen_profile_context = Vec::new();

        if !self.domain.is_empty() {
            remember_context_value(&mut seen_profile_context, &self.domain);
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!(
                    "[agent:domain] Specialization/background: {}. Use this context to be helpful, but do not treat it as a refusal policy. Related adjacent tasks are allowed unless explicit invariants forbid them.",
                    self.domain
                ),
            });
        }

        if let Some(profile) = self.agent_profile.as_ref()
            && let Some(block) = render_profile_block(profile)
        {
            remember_agent_profile_context(&mut seen_profile_context, profile);
            blocks.push(ChatMessage {
                role: Role::System,
                content: block,
            });
        }

        if let Some(task) = self.task_state.as_ref() {
            blocks.push(ChatMessage {
                role: Role::System,
                content: render_task_block(task),
            });
            if !task.violations.is_empty() {
                blocks.push(ChatMessage {
                    role: Role::System,
                    content: format!(
                        "[invariants] Your previous response violated these invariants — correct this now:\n- {}",
                        task.violations.join("\n- ")
                    ),
                });
            }
        }

        if !self.invariants.is_empty() {
            for invariant in &self.invariants {
                remember_context_value(&mut seen_profile_context, invariant);
            }
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!(
                    "[invariants] These constraints are absolute and must never be broken, even if the user asks:\n- {}",
                    self.invariants.join("\n- ")
                ),
            });
        }

        if let Some(profile) = self.user_profile.as_ref()
            && let Some(block) = render_user_profile_block(profile, &seen_profile_context)
        {
            blocks.push(ChatMessage {
                role: Role::System,
                content: block,
            });
        }

        if blocks.is_empty() {
            return;
        }
        let insert_at = messages
            .iter()
            .position(|message| message.role != Role::System)
            .unwrap_or(messages.len());
        for (offset, block) in blocks.into_iter().enumerate() {
            messages.insert(insert_at + offset, block);
        }
    }

    fn request_with_user_prompt(&self, prompt: String) -> ChatRequest {
        let mut memory_config = self.memory_config.clone();
        if memory_config.strategy == MemoryStrategy::StickyFacts {
            // The current user prompt is part of "last N messages", so only
            // N-1 previous non-system messages are pulled from history here.
            memory_config.recent_messages = memory_config.recent_messages.saturating_sub(1);
        }
        let mut messages = self.memory.build_context(&self.history, &memory_config);
        self.inject_memory_layers(&mut messages);
        messages.push(ChatMessage {
            role: Role::User,
            content: prompt,
        });
        ChatRequest {
            model: self.profile.model.clone(),
            messages,
            control: self.control.clone(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        }
    }

    fn commit_turn(&mut self, prompt: String, answer: String) {
        let user_index = self.history.len();
        self.history.push(ChatMessage {
            role: Role::User,
            content: prompt,
        });
        let assistant_index = self.history.len();
        self.history.push(ChatMessage {
            role: Role::Assistant,
            content: answer,
        });
        self.memory
            .record_turn_branch(user_index, assistant_index, &self.memory_config);
    }
}

pub(crate) fn memory_summary_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.2);
    control.max_tokens = Some(700);
    control
}

pub(crate) fn memory_facts_extraction_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.0);
    control.max_tokens = Some(500);
    control
}

pub(crate) fn topic_classifier_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.0);
    control.max_tokens = Some(250);
    control
}

pub(crate) fn parse_topic_route_decision(text: &str) -> Option<TopicRouteDecision> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let object = value.as_object()?;
    let found = object.get("found")?.as_bool()?;
    let topic_id = object
        .get("topic_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let confidence = object
        .get("confidence")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0) as f32;
    let reason = object
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    Some(TopicRouteDecision {
        found,
        topic_id,
        confidence,
        reason,
    })
}

pub(crate) fn topic_not_found_message(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "Не нашёл подходящий topic-файл для этого сообщения. Контекст другой темы не подгружал."
            .to_string()
    } else {
        format!(
            "Не нашёл подходящий topic-файл для этого сообщения. Контекст другой темы не подгружал. Причина: {reason}"
        )
    }
}

pub(crate) fn local_chat_response(text: String, metrics: Option<RequestMetrics>) -> ChatResponse {
    ChatResponse {
        text,
        finish_reason: Some("topic_not_found".to_string()),
        metrics: metrics.unwrap_or_default(),
    }
}

pub(crate) fn local_provider_debug() -> ProviderExchangeDebug {
    ProviderExchangeDebug {
        request: crate::providers::HttpDebugRequest {
            method: "LOCAL".to_string(),
            url: "local://topic-file-routing/not-found".to_string(),
            headers: Default::default(),
            body: serde_json::json!({}),
        },
        response: crate::providers::HttpDebugResponse {
            status: 200,
            headers: Default::default(),
            body: serde_json::json!({ "finish_reason": "topic_not_found" }),
        },
    }
}

/// One extracted fact plus the layer the agent explicitly chose for it.
/// `layer` is `None` when the model gave no layer (legacy/flat output); the
/// caller then falls back to the default routing rule.
pub(crate) struct ExtractedFact {
    pub key: String,
    pub value: String,
    pub layer: Option<MemoryLayer>,
}

/// Parse the fact-extraction response. The agent is asked to classify each
/// durable fact into a memory layer, so several shapes are accepted:
///
/// * `{"facts":[{"key","value","layer"}]}` — preferred, explicit layer.
/// * `[{"key","value","layer"}]` — bare array.
/// * `{"key":{"value":"...","layer":"..."}}` — object of rich entries.
/// * `{"key":"value"}` — legacy flat object (no layer → default routing).
pub(crate) fn parse_extracted_facts(text: &str) -> Option<Vec<ExtractedFact>> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let entries = match &value {
        serde_json::Value::Array(items) => return Some(facts_from_entry_array(items)),
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::Array(items)) = object.get("facts") {
                return Some(facts_from_entry_array(items));
            }
            object
        }
        _ => return None,
    };
    let mut facts = Vec::new();
    for (key, raw) in entries {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let (value, layer) = match raw {
            serde_json::Value::Object(entry) => {
                let Some(value) = entry.get("value").and_then(fact_value_to_string) else {
                    continue;
                };
                (value, entry.get("layer").and_then(parse_fact_layer))
            }
            other => {
                let Some(value) = fact_value_to_string(other) else {
                    continue;
                };
                (value, None)
            }
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        facts.push(ExtractedFact {
            key: key.to_string(),
            value: value.to_string(),
            layer,
        });
    }
    Some(facts)
}

fn facts_from_entry_array(items: &[serde_json::Value]) -> Vec<ExtractedFact> {
    let mut facts = Vec::new();
    for item in items {
        let Some(entry) = item.as_object() else {
            continue;
        };
        let Some(key) = entry.get("key").and_then(|value| value.as_str()) else {
            continue;
        };
        let key = key.trim();
        let Some(value) = entry.get("value").and_then(fact_value_to_string) else {
            continue;
        };
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        facts.push(ExtractedFact {
            key: key.to_string(),
            value: value.to_string(),
            layer: entry.get("layer").and_then(parse_fact_layer),
        });
    }
    facts
}

fn parse_fact_layer(value: &serde_json::Value) -> Option<MemoryLayer> {
    value.as_str()?.parse::<MemoryLayer>().ok()
}

pub(crate) fn task_title_from_prompt(prompt: &str) -> String {
    let cleaned = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(prompt)
        .trim_matches(['"', '\'', '`', '*', '#', ' ']);
    let first_sentence = cleaned
        .split(['.', '!', '?', '\n'])
        .next()
        .unwrap_or(cleaned)
        .trim();
    let mut title = first_sentence.chars().take(80).collect::<String>();
    if first_sentence.chars().count() > 80 {
        title.push('…');
    }
    title
}

pub(crate) fn task_decision_from_exchange(user_prompt: &str, answer: &str) -> Option<String> {
    let user_lower = user_prompt.to_lowercase();
    let answer_lower = answer.to_lowercase();
    let is_approval = ["утверждаем", "самое то", "выбор сделан", "останавливаемся"]
        .iter()
        .any(|marker| user_lower.contains(marker) || answer_lower.contains(marker));
    if !is_approval {
        return None;
    }
    let candidate = user_prompt
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .find(|part| {
            !part.is_empty()
                && !part.to_lowercase().contains("утверждаем")
                && !part.to_lowercase().contains("самое то")
        })
        .or_else(|| {
            answer
                .split(['\n', '.', '!', '?'])
                .map(str::trim)
                .find(|part| !part.is_empty())
        })?;
    Some(format!("Approved decision: {candidate}"))
}

fn contains_any_agent(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

/// Deterministic task-state inferred from one exchange (keyword-based, no LLM).
/// Used by the orchestrator to keep stage + progress fields tracking the user's
/// actual request.
#[derive(Debug, Clone)]
pub(crate) struct InferredTaskState {
    pub(crate) stage: TaskStage,
    pub(crate) current_step: String,
    pub(crate) expected_action: String,
    pub(crate) resume_hint: String,
}

/// Map the latest exchange to a task stage + progress fields by intent keywords.
/// Returns `None` when nothing clearly indicates a stage.
pub(crate) fn infer_task_state_from_exchange(
    current: TaskStage,
    user_prompt: &str,
    answer: &str,
) -> Option<InferredTaskState> {
    let user = user_prompt.to_lowercase();
    let combined = format!("{user}\n{}", answer.to_lowercase());
    let wants_done = contains_any_agent(
        &user,
        &[
            "итог",
            "резюм",
            "заверш",
            "готово",
            "следующие действия",
            "что готово",
            "что отправ",
            "final",
            "summary",
            "done",
        ],
    );
    let wants_validation = contains_any_agent(
        &user,
        &[
            "проверь",
            "провер",
            "риски",
            "слабые места",
            "не хватает",
            "нельзя утверждать",
            "валидац",
            "review",
            "validate",
        ],
    );
    let wants_execution = contains_any_agent(
        &user,
        &[
            "составь",
            "подготовь",
            "напиши",
            "набросать",
            "набросай",
            "черновик",
            "шаблон",
            "сформируй",
            "draft",
            "write",
        ],
    );
    let wants_planning = contains_any_agent(
        &combined,
        &[
            "план",
            "что делать",
            "досудебн",
            "претензи",
            "документ",
            "срок оплат",
            "акт подписан",
            "заказчик призна",
            "next step",
            "plan",
        ],
    );

    let mut stage = if wants_done {
        TaskStage::Done
    } else if wants_validation {
        TaskStage::Validation
    } else if wants_execution {
        TaskStage::Execution
    } else if wants_planning {
        TaskStage::Planning
    } else {
        return None;
    };

    if !current.can_transition(stage) {
        stage = match (current, stage) {
            (TaskStage::Clarify, TaskStage::Execution | TaskStage::Validation) => {
                TaskStage::Planning
            }
            (TaskStage::Planning, TaskStage::Validation) if wants_validation => {
                TaskStage::Validation
            }
            (TaskStage::Execution, TaskStage::Done) if wants_done => TaskStage::Done,
            (TaskStage::Execution, TaskStage::Planning) if wants_planning => TaskStage::Planning,
            (TaskStage::Validation, TaskStage::Execution) if wants_execution => {
                TaskStage::Execution
            }
            (TaskStage::Validation, TaskStage::Done) if wants_done => TaskStage::Done,
            _ => current,
        };
    }

    let (current_step, expected_action, resume_hint) = match stage {
        TaskStage::Clarify => (
            "уточнить недостающие факты",
            "user_input",
            "продолжить с уточнения фактов задачи",
        ),
        TaskStage::Planning => (
            "составить план действий и список нужных документов",
            "agent_work",
            "продолжить с плана и недостающих данных",
        ),
        TaskStage::Execution => (
            "подготовить рабочий черновик результата",
            "agent_work",
            "продолжить с подготовки черновика",
        ),
        TaskStage::Validation => (
            "проверить риски, пробелы и ограничения",
            "validation",
            "продолжить с проверки рисков и недостающих данных",
        ),
        TaskStage::Done => (
            "зафиксировать итог и следующие действия",
            "none",
            "задача завершена",
        ),
    };

    Some(InferredTaskState {
        stage,
        current_step: current_step.to_string(),
        expected_action: expected_action.to_string(),
        resume_hint: resume_hint.to_string(),
    })
}

pub(crate) fn paused_user_supplied_next_info(prompt: &str) -> bool {
    contains_any_agent(
        prompt,
        &[
            "наш",
            "вот",
            "договор",
            "реквизит",
            "адрес",
            "номер",
            "ооо",
            "акт",
            "пункт",
            "срок",
            "нашёл",
            "нашел",
        ],
    )
}

/// A proposed task-stage transition and whether the FSM accepted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTransition {
    pub from: TaskStage,
    pub to: TaskStage,
    pub accepted: bool,
}

/// Result of one turn's stateful post-processing, surfaced to the debug view.
#[derive(Debug, Clone, Default)]
pub struct StatefulReport {
    /// Required profile fields the agent still needs to ask about.
    pub pending_questions: Vec<String>,
    /// Current task stage after processing (None when no task is active).
    pub stage: Option<TaskStage>,
    pub current_step: String,
    pub expected_action: String,
    pub paused: bool,
    pub resume_hint: String,
    /// The transition decided this turn, if any (includes rejected ones).
    pub stage_transition: Option<StageTransition>,
    /// Invariants the last response violated (empty = clean).
    pub violations: Vec<String>,
    /// Code-level invariant gate result: not_run, pass, blocked, or unverified.
    pub invariant_status: String,
    /// Safe summary for UI/debug. This is a decision trace, not chain-of-thought.
    pub invariant_summary: String,
    /// Titles (or goals) of the paused tasks tracked in parallel with the active
    /// one. Empty when no other task is on the board.
    pub backlog: Vec<String>,
    /// Combined token metrics of the auxiliary LLM calls.
    pub metrics: Option<RequestMetrics>,
}

/// Generate an interview schema from a domain description. The model proposes the
/// fields the agent should elicit; `seed_fields` are forced in as required.
pub async fn build_profile_schema(
    client: &dyn ProviderClient,
    profile: &ProfileConfig,
    token: &str,
    domain: &str,
    seed_fields: &[String],
) -> Result<Vec<ProfileField>, AppError> {
    let seed = seed_fields
        .iter()
        .map(|field| field.trim())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let seed_hint = if seed.is_empty() {
        String::new()
    } else {
        format!(
            "\nThese fields are required, include them: {}.",
            seed.join(", ")
        )
    };
    let request = ChatRequest {
        model: profile.model.clone(),
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "You design a short onboarding interview for a specialised assistant. Given a domain, list the profile fields the assistant should learn about the user/project (stack, audience, constraints, goals, level, …). Reply with JSON array of {\"key\":\"snake_case\",\"question\":\"...\",\"required\":true|false}. 4-8 fields. No prose.".to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: format!("Domain:\n{domain}{seed_hint}"),
            },
        ],
        control: memory_facts_extraction_control(),
        pricing: None,
        billing: None,
    };
    let response = client.chat_completion(profile, token, request).await?;
    // The seed fields are injected into the prompt as required, so the model
    // folds them into the schema with proper keys/questions; the user can still
    // add or edit fields in the UI.
    Ok(parse_profile_schema(response.text.as_str()))
}

fn normalize_schema_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// Parse the schema-builder response (`[{key,question,required}]`).
fn parse_profile_schema(text: &str) -> Vec<ProfileField> {
    let Some(value) = extract_json_value(text) else {
        return Vec::new();
    };
    let entries = match &value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map
            .get("fields")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let key = normalize_schema_key(object.get("key")?.as_str()?);
            if key.is_empty() {
                return None;
            }
            let question = object
                .get("question")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let required = object
                .get("required")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            Some(ProfileField {
                key,
                question,
                required,
                value: String::new(),
            })
        })
        .collect()
}

/// Parse `{key: value}` profile updates from the fill-extraction response.
pub(crate) fn parse_profile_updates(text: &str) -> Vec<(String, String)> {
    let Some(serde_json::Value::Object(map)) = extract_json_value(text) else {
        return Vec::new();
    };
    map.into_iter()
        .filter_map(|(key, value)| fact_value_to_string(&value).map(|value| (key, value)))
        .collect()
}

/// Parse a strict invariant-check response. `None` means the checker did not
/// return the required schema, which must be handled fail-closed for invariants
/// that code cannot evaluate deterministically.
pub(crate) fn parse_invariant_check(text: &str) -> Option<Vec<String>> {
    let raw = text.trim();
    let trimmed = if let Some(body) = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```JSON"))
        .or_else(|| raw.strip_prefix("```"))
    {
        body.strip_suffix("```")?.trim()
    } else {
        raw
    };
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    let array = match value {
        serde_json::Value::Object(map) => map
            .get("violations")
            .and_then(|value| value.as_array())
            .cloned()?,
        _ => return None,
    };
    let violations = array
        .iter()
        .map(|item| item.as_str())
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Some(violations)
}

pub(crate) fn local_invariant_violations(invariants: &[String], answer: &str) -> Vec<String> {
    local_invariant_check(invariants, answer).violations
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalInvariantCheck {
    pub violations: Vec<String>,
    /// Invariants whose semantics are fully covered by deterministic code.
    pub verified: Vec<String>,
    /// Invariants that need the semantic checker. If it is unavailable or
    /// malformed, the orchestrator blocks the pending answer.
    pub unknown: Vec<String>,
}

pub(crate) fn local_invariant_check(invariants: &[String], answer: &str) -> LocalInvariantCheck {
    let answer_lower = answer.to_lowercase();
    let has_legal_citation = contains_any_agent(
        &answer_lower,
        &["ст.", "статья", "гк рф", "апк рф", "фз", "коап", "ук рф"],
    );
    let russian_enough = mostly_russian_prose(answer);
    let legal_conclusion_claim = contains_non_negated_phrase(
        &answer_lower,
        &["юридическое заключение", "юридическим заключением"],
    );

    let mut result = LocalInvariantCheck::default();
    for invariant in invariants {
        let normalized = invariant.to_lowercase();
        let violation = if normalized.contains("отвечать") && normalized.contains("рус")
        {
            Some(!russian_enough)
        } else if normalized.contains("не выдумывать") && normalized.contains("норм")
        {
            Some(has_legal_citation)
        } else if normalized.contains("отделять факты") && normalized.contains("предполож")
        {
            // Positive presentation rule: absence of literal headings is not a
            // violation. Code blocks only explicit refusal to distinguish them;
            // nuanced quality remains the semantic checker's job.
            Some(explicit_fact_assumption_mixing(&answer_lower))
        } else if normalized.contains("недоста") && normalized.contains("риск") {
            Some(explicit_risk_document_omission(&answer_lower))
        } else if normalized.contains("сроч") && normalized.contains("риск") {
            Some(explicit_urgency_omission(&answer_lower))
        } else if normalized.contains("не называть")
            && normalized.contains("юридическим заключением")
        {
            Some(legal_conclusion_claim)
        } else if let Some(allowed) = extract_only_allowed_technologies(&normalized) {
            Some(mentions_disallowed_technology(&answer_lower, &allowed))
        } else if normalized.contains("не обещ")
            && contains_any_agent(&normalized, &["исход", "результат", "побед"])
        {
            Some(contains_any_agent(
                &answer_lower,
                &[
                    "гарантир",
                    "точно выигр",
                    "обязательно побед",
                    "100%",
                    "сто процентов",
                ],
            ))
        } else if normalized.contains("не отправ")
            && normalized.contains("проверк")
            && normalized.contains("реквизит")
        {
            let recommends_send =
                contains_any_agent(&answer_lower, &["отправляйте", "отправьте", "направьте"]);
            let requires_check = answer_lower.contains("провер")
                && (answer_lower.contains("реквизит") || answer_lower.contains("данн"));
            Some(recommends_send && !requires_check)
        } else if let Some(forbidden) = extract_forbidden_literal(&normalized) {
            Some(contains_non_negated_phrase(
                &answer_lower,
                &[forbidden.as_str()],
            ))
        } else {
            None
        };

        match violation {
            Some(true) => {
                result.verified.push(invariant.clone());
                result.violations.push(invariant.clone());
            }
            Some(false) => result.verified.push(invariant.clone()),
            None => result.unknown.push(invariant.clone()),
        }
    }
    result
}

fn explicit_fact_assumption_mixing(answer: &str) -> bool {
    contains_any_agent(
        answer,
        &[
            "не нужно отделять факты",
            "не буду отделять факты",
            "считаем предположение фактом",
            "предположение — это факт",
            "предположение это факт",
        ],
    )
}

fn explicit_risk_document_omission(answer: &str) -> bool {
    contains_any_agent(
        answer,
        &[
            "рисков нет",
            "документы не нужны",
            "ничего проверять не нужно",
            "недостающих документов нет",
        ],
    )
}

fn explicit_urgency_omission(answer: &str) -> bool {
    if contains_any_agent(
        answer,
        &[
            "срочность отмечать не нужно",
            "срочности нет",
            "можно не торопиться",
            "не нужно спешить",
        ],
    ) {
        return true;
    }
    let established_deadline = contains_any_agent(
        answer,
        &[
            "крайний срок",
            "срок истекает",
            "до обращения в суд осталось",
            "срок исковой давности истекает",
            "процессуальный срок истекает",
        ],
    );
    if !established_deadline {
        return false;
    }
    let has_urgency = contains_any_agent(
        answer,
        &["сроч", "не затяг", "немедлен", "как можно скорее"],
    );
    !has_urgency
}

/// A safe assistant refusal/compliance explanation is not business-task work.
/// The orchestrator uses this to preserve task title/goal/stage/artifacts.
pub(crate) fn is_invariant_compliance_response(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    let refuses = contains_any_agent(
        &lower,
        &[
            "не могу выполнить этот запрос",
            "не могу выполнить просьбу",
            "не могу выдать этот ответ",
            "ответ заблокирован на уровне кода",
            "нарушает инвариант",
            "нарушает два абсолютных инварианта",
            "нарушает установленные ограничения",
        ],
    );
    let names_policy = lower.contains("инвариант")
        || lower.contains("ограничен")
        || lower.contains("обязан отвечать")
        || lower.contains("не могу придумывать");
    refuses && names_policy
}

fn mostly_russian_prose(text: &str) -> bool {
    let mut in_code = false;
    let mut cyrillic = 0usize;
    let mut latin = 0usize;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        for ch in line.chars() {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if matches!(lower, 'а'..='я' | 'ё') {
                cyrillic += 1;
            } else if lower.is_ascii_alphabetic() {
                latin += 1;
            }
        }
    }
    cyrillic >= 4 && cyrillic >= latin
}

fn contains_non_negated_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| {
        text.match_indices(phrase).any(|(index, _)| {
            let clause_start = text[..index]
                .rmatch_indices(['.', '!', '?', '\n', ';'])
                .next()
                .map(|(offset, delimiter)| offset + delimiter.len())
                .unwrap_or(0);
            let phrase_end = index + phrase.len();
            let clause_end = text[phrase_end..]
                .match_indices(['.', '!', '?', '\n', ';'])
                .next()
                .map(|(offset, _)| phrase_end + offset)
                .unwrap_or(text.len());
            let context = &text[clause_start..clause_end];
            !contains_any_agent(
                context,
                &[
                    "не является",
                    "не юридическ",
                    "не счита",
                    "не называ",
                    "не использу",
                    "не примен",
                    "не предлага",
                    "не обеща",
                    "без ",
                    "отказ от",
                    "исключить",
                    "запрещено",
                ],
            )
        })
    })
}

fn extract_forbidden_literal(invariant: &str) -> Option<String> {
    let trimmed = invariant.trim().trim_end_matches(['.', ';', ':']).trim();
    for prefix in [
        "без ",
        "не использовать ",
        "не применять ",
        "не предлагать ",
        "запрещено использовать ",
        "нельзя использовать ",
    ] {
        if let Some(value) = trimmed.strip_prefix(prefix) {
            let value = value.trim();
            if !value.is_empty() && value.split_whitespace().count() <= 8 {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_only_allowed_technologies(invariant: &str) -> Option<Vec<String>> {
    let value = [
        "архитектура только ",
        "стек только ",
        "использовать только ",
        "только ",
    ]
    .iter()
    .find_map(|prefix| invariant.strip_prefix(prefix))?;
    let allowed: Vec<String> = known_technologies()
        .iter()
        .filter(|technology| contains_standalone_term(value, technology))
        .map(|technology| (*technology).to_string())
        .collect();
    (!allowed.is_empty()).then_some(allowed)
}

fn mentions_disallowed_technology(answer: &str, allowed: &[String]) -> bool {
    known_technologies().iter().any(|technology| {
        !allowed.iter().any(|item| item.as_str() == *technology)
            && contains_standalone_term(answer, technology)
            && contains_non_negated_phrase(answer, &[*technology])
    })
}

fn contains_standalone_term(text: &str, term: &str) -> bool {
    text.match_indices(term).any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + term.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn known_technologies() -> &'static [&'static str] {
    &[
        "angular",
        "axum",
        "coroutines",
        "dart",
        "django",
        "express",
        "fastapi",
        "flutter",
        "java",
        "javascript",
        "kotlin",
        "ktor",
        "mvc",
        "mvi",
        "mvvm",
        "nestjs",
        "node.js",
        "python",
        "react",
        "rust",
        "rxjava",
        "spring",
        "swift",
        "swiftui",
        "typescript",
        "vue",
    ]
}

/// Deterministic refusal shown when a response still breaks hard invariants
/// after the bounded retries. Invariants live outside the dialog and outrank the
/// current request, so the orchestrator blocks the violating answer instead of
/// returning it (lecture rule: the assistant refuses to propose solutions that
/// break invariants, and explains why). Code-level, never an LLM decision.
pub(crate) fn build_invariant_refusal_for_prompt(
    violations: &[String],
    user_prompt: &str,
) -> String {
    let mut out = String::from(
        "⛔ Не могу выдать этот ответ: он нарушает инварианты задачи.\n\nНарушенные инварианты:\n",
    );
    for violation in violations {
        let line = violation.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str("• ");
        out.push_str(line);
        out.push('\n');
    }
    let prompt = user_prompt.trim();
    if !prompt.is_empty() {
        out.push_str("\nКонфликт: текущая просьба требует результата, который нельзя безопасно выдать при этих ограничениях.");
    }
    out.push_str(
        "\nПочему отказ: инварианты зафиксированы отдельно от диалога и не меняются \
от запроса к запросу — они приоритетнее текущей просьбы. Я переработал ответ, но \
снять нарушение не удалось.\n\nЧто можно сделать: переформулируйте запрос в рамках \
ограничений или измените сам инвариант, если это допустимо.",
    );
    out
}

pub(crate) fn build_invariant_unverified_response(invariants: &[String]) -> String {
    let mut out = String::from(
        "⚠️ Я не могу безопасно выдать подготовленный ответ: проверка обязательных инвариантов не завершилась, поэтому ответ заблокирован на уровне кода.\n\nНужно подтвердить:\n",
    );
    for invariant in invariants {
        let line = invariant.trim();
        if !line.is_empty() {
            out.push_str("• ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(
        "\nУточните ограничение более формально либо повторите запрос — я заново проверю ответ. Сам неподтверждённый вариант пользователю не показывается.",
    );
    out
}

/// Whether the user's prompt starts a brand-new task (vs. continuing the current
/// one). Used by the orchestrator to reset the FSM to `Clarify` — including from
/// the terminal `Done` stage, which otherwise has no outgoing transition. Pure
/// keyword heuristic, no LLM.
pub(crate) fn starts_new_task(current: TaskStage, prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let explicit = contains_any_agent(
        &lower,
        &[
            "новая задача",
            "новую задачу",
            "другая задача",
            "другую задачу",
            "ещё одна задача",
            "еще одна задача",
            "ещё одну задачу",
            "еще одну задачу",
            "следующая задача",
            "следующую задачу",
            "давай теперь",
            "теперь сделаем",
            "теперь составим",
            "теперь подготов",
            "теперь напиш",
            "теперь оформ",
            "займёмся",
            "займемся",
            "перейдём к",
            "перейдем к",
            "new task",
            "next task",
            "another task",
        ],
    );
    // At the terminal stage any fresh actionable request is necessarily a new
    // task: the current one is finished and cannot advance further.
    let done_with_intent = current == TaskStage::Done
        && infer_task_state_from_exchange(TaskStage::Clarify, prompt, "").is_some();
    explicit || done_with_intent
}

/// Whether the prompt asks to switch back to a paused task already on the board.
/// Matches an explicit "back to" cue plus an overlap with that task's title/goal,
/// so the orchestrator can resume the right one. Pure keyword heuristic, no LLM.
pub(crate) fn switch_back_target(
    prompt: &str,
    backlog: &[super::store::TaskContext],
) -> Option<usize> {
    let lower = prompt.to_lowercase();
    let wants_switch = contains_any_agent(
        &lower,
        &[
            "вернёмся к",
            "вернемся к",
            "вернуться к",
            "назад к",
            "обратно к",
            "продолжим задачу",
            "продолжить задачу",
            "back to",
            "switch to",
            "resume task",
        ],
    );
    if !wants_switch {
        return None;
    }
    backlog
        .iter()
        .enumerate()
        .filter_map(|(index, task)| {
            let score = task_reference_overlap(&lower, task);
            (score > 0).then_some((index, score))
        })
        .max_by_key(|(_, score)| *score)
        .map(|(index, _)| index)
}

/// Count significant title/goal words from `task` that appear in the prompt.
fn task_reference_overlap(prompt_lower: &str, task: &super::store::TaskContext) -> usize {
    let mut haystack = task.title.to_lowercase();
    haystack.push(' ');
    haystack.push_str(&task.goal.to_lowercase());
    haystack
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .filter(|word| prompt_lower.contains(*word))
        .count()
}

/// Last-resort guard for legal-agent answers. If a model keeps asserting an
/// unverified legal rule after bounded retries, replace the unsafe line with a
/// transparent verification note instead of returning the violation.
pub(crate) fn sanitize_unverified_legal_claims(
    invariants: &[String],
    answer: &str,
    known_user_context: &str,
) -> String {
    let requires_verified_norms = invariants.iter().any(|invariant| {
        let normalized = invariant.to_lowercase();
        normalized.contains("не выдумывать") && normalized.contains("норм")
    });
    if !requires_verified_norms {
        return answer.to_string();
    }

    let mut sanitized = answer.to_string();
    let known_payment_term = known_payment_term(known_user_context);
    for year in unverified_years(answer, known_user_context) {
        sanitized = sanitized.replace(&year, "[год]");
    }

    let mut replaced = false;
    let mut lines = Vec::new();
    let known_lower = known_user_context.to_lowercase();
    for line in sanitized.lines() {
        let lower = line.to_lowercase();
        if known_lower.contains("заказчик ооо")
            && (lower.contains("не знаете") || lower.contains("неизвест"))
            && (lower.contains("должник") || lower.contains("ооо или ип"))
        {
            lines.push("Статус должника подтверждён пользователем: заказчик — ООО.".to_string());
            continue;
        }
        if known_lower.contains("хочу начать без суда") && lower.contains("вы направили претензи")
        {
            lines.push(
                "Претензия ещё не направлена; после подготовки нужно сохранить подтверждение отправки."
                    .to_string(),
            );
            continue;
        }
        if lower.contains("адрес") {
            let mut safe_address = line.to_string();
            for number in numeric_tokens(&lower) {
                if number.len() == 6 && !known_user_context.contains(&number) {
                    safe_address = safe_address.replace(&number, "[почтовый индекс]");
                }
            }
            if safe_address != line {
                lines.push(safe_address);
                continue;
            }
        }
        let has_unknown_number = numeric_tokens(&lower)
            .into_iter()
            .any(|number| !known_user_context.contains(&number));
        if lower.contains("с момента получения")
            && lower.contains("претензи")
            && lower.chars().any(|ch| ch.is_ascii_digit())
        {
            let prefix = leading_list_prefix(line).unwrap_or("1.");
            lines.push(format!(
                "{prefix} В течение [срок требования] с момента получения настоящей претензии перечислить задолженность по указанным реквизитам."
            ));
            continue;
        }
        if lower.contains("акт")
            && lower.contains("например")
            && lower.chars().any(|ch| ch.is_ascii_digit())
        {
            lines
                .push("Акт приёма-передачи подписан сторонами [дата подписания акта].".to_string());
            continue;
        }
        let has_citation = contains_any_agent(
            &lower,
            &["ст.", "статья", "гк рф", "апк рф", "фз", "коап", "ук рф"],
        );
        let has_unverified_legal_number = (lower.contains("госпошлин")
            || lower.contains("судебн") && lower.contains("приказ")
            || lower.contains("срок хранения")
            || lower.contains("срок исковой")
            || lower.contains("общий срок") && lower.contains("год")
            || lower.contains("досудебн") && lower.contains("обязател")
            || lower.contains("срок") && lower.contains("дн")
            || lower.contains("процент") && lower.contains("ставк")
            || lower.contains("требован") && lower.contains("дн")
            || lower.contains("потреб") && lower.contains("дн")
            || lower.contains("оплатить") && lower.contains("дн")
            || lower.contains("обычно") && lower.contains("дн")
            || lower.contains("производств")
            || lower.contains("подсудн"))
            && has_unknown_number;
        let has_unverified_legal_absolute =
            (lower.contains("досудебн") || lower.contains("претензи") || lower.contains("суд"))
                && (lower.contains("обязател")
                    || lower.contains("оставит иск")
                    || lower.contains("оставить иск")
                    || lower.contains("без рассмотрения")
                    || lower.contains("откаж")
                    || lower.contains("не сможете")
                    || lower.contains("только ценн")
                    || lower.contains("только заказн"));
        let has_unverified_timeline = lower.contains("год-полтора")
            || lower.contains("полтора год")
            || ((lower.contains("месяц") || lower.contains(" лет")) && has_unknown_number);
        if has_citation
            || has_unverified_legal_number
            || has_unverified_legal_absolute
            || has_unverified_timeline
        {
            if !replaced {
                lines.push(
                    "Применимую норму права, порядок взыскания и суммы санкций нужно проверить по договору и актуальной редакции закона."
                        .to_string(),
                );
                replaced = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }
    let mut result = lines.join("\n");
    if let Some(term) = known_payment_term {
        let term_numbers = numeric_tokens(&term);
        let preserved = term_numbers.iter().all(|number| result.contains(number))
            && result.to_lowercase().contains("оплат");
        if !preserved {
            result = format!("Подтверждённое условие договора: {term}\n{result}");
        }
    }
    result
}

fn leading_list_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let end = trimmed.find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != ')')?;
    let candidate = trimmed[..end].trim();
    (!candidate.is_empty()
        && candidate.chars().any(|ch| ch.is_ascii_digit())
        && (candidate.ends_with('.') || candidate.ends_with(')')))
    .then_some(candidate)
}

fn known_payment_term(context: &str) -> Option<String> {
    context
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .find(|part| {
            let lower = part.to_lowercase();
            lower.contains("оплат")
                && lower.chars().any(|ch| ch.is_ascii_digit())
                && (lower.contains("дн") || lower.contains("рабоч"))
        })
        .map(|part| {
            part.split_once(':')
                .filter(|(prefix, _)| prefix.split_whitespace().count() <= 3)
                .map(|(_, value)| value.trim())
                .unwrap_or(part)
                .to_string()
        })
}

fn numeric_tokens(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut digits = String::new();
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            values.push(std::mem::take(&mut digits));
        }
    }
    values
}

fn unverified_years(answer: &str, known_user_context: &str) -> Vec<String> {
    let mut years = Vec::new();
    let mut digits = String::new();
    for ch in answer.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if digits.len() == 4
            && digits
                .parse::<u16>()
                .is_ok_and(|year| (1900..=2100).contains(&year))
            && !known_user_context.contains(&digits)
            && !years.contains(&digits)
        {
            years.push(digits.clone());
        }
        digits.clear();
    }
    years
}

/// Best-effort JSON extraction: parse directly, else grab the first `{...}` or
/// `[...]` span (handles models that wrap JSON in prose or code fences).
fn extract_json_value(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim().trim_start_matches("```json").trim_matches('`');
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed.trim()) {
        return Some(value);
    }
    let bytes = trimmed.as_bytes();
    for (open, close) in [(b'{', b'}'), (b'[', b']')] {
        if let (Some(start), Some(end)) = (
            bytes.iter().position(|&b| b == open),
            bytes.iter().rposition(|&b| b == close),
        ) && end > start
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&trimmed[start..=end])
        {
            return Some(value);
        }
    }
    None
}

/// Render the working-memory task block: stage FSM + plan as read-only context.
/// Stage order is bookkeeping for the tracked task, not a gate on adjacent user
/// requests in the same agent.
pub(crate) fn render_task_block(task: &super::store::TaskContext) -> String {
    let mut lines = vec![format!("Stage: {}", task.stage)];
    lines.push(format!("Paused: {}", task.paused));
    lines.push(
        "Use this as authoritative working memory for the tracked task. Treat fragmentary follow-up messages as continuation of this task unless the user clearly starts an unrelated one."
            .to_string(),
    );
    lines.push(
        "Process rules: work only inside the current stage/current step, do not skip stages, and let the task sub-agent advance the FSM only after the current stage output is actually produced."
            .to_string(),
    );
    if task.paused {
        lines.push(
            "Task is paused. If the user asks to resume or continue, use the saved step/action/hint below and do not ask them to restate the task."
                .to_string(),
        );
    }
    let allowed = task.stage.allowed_next();
    if allowed.is_empty() {
        lines.push("Tracked task status: done.".to_string());
    } else {
        let names: Vec<String> = allowed.iter().map(|stage| stage.to_string()).collect();
        lines.push(format!(
            "Tracked-task next stages: {}. Use these only for internal task-state updates.",
            names.join(", ")
        ));
    }
    for (label, value) in [
        ("Current step", task.current_step.as_str()),
        ("Expected action", task.expected_action.as_str()),
        ("Resume hint", task.resume_hint.as_str()),
    ] {
        let value = value.trim();
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    for (label, value) in [("Title", task.title.as_str()), ("Goal", task.goal.as_str())] {
        let value = value.trim();
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    let plan: Vec<&str> = task
        .plan
        .iter()
        .map(|step| step.trim())
        .filter(|step| !step.is_empty())
        .collect();
    if !plan.is_empty() {
        lines.push(format!("Plan:\n- {}", plan.join("\n- ")));
    }
    if !task.pipeline.is_empty() {
        lines.push(render_pipeline_block(task));
    }
    let artifacts: Vec<String> = task
        .artifacts
        .iter()
        .filter_map(|artifact| {
            let key = artifact.key.trim();
            let value = artifact.value.trim();
            if key.is_empty() && value.is_empty() {
                None
            } else {
                Some(format!("{}:{} = {}", artifact.stage, key, value))
            }
        })
        .collect();
    if !artifacts.is_empty() {
        lines.push(format!("Pipeline artifacts:\n- {}", artifacts.join("\n- ")));
    }
    let results: Vec<&str> = task
        .results
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect();
    if !results.is_empty() {
        lines.push(format!("Results so far:\n- {}", results.join("\n- ")));
    }
    let notes = task.notes.trim();
    if !notes.is_empty() {
        lines.push(format!("Notes: {notes}"));
    }
    format!("[memory:working] Task state:\n{}", lines.join("\n"))
}

fn render_pipeline_block(task: &super::store::TaskContext) -> String {
    let mut lines = Vec::new();
    for stage in &task.pipeline {
        let mut line = format!("- {} ({})", stage.name.trim(), stage.stage);
        if stage.name.trim().is_empty() {
            line = format!("- {}", stage.stage);
        }
        if stage.requires_human_approval {
            line.push_str(" [requires human approval]");
        }
        let artifact_key = stage.artifact_key.trim();
        if !artifact_key.is_empty() {
            line.push_str(&format!(" -> artifact: {artifact_key}"));
        }
        let system_prompt = stage.system_prompt.trim();
        if !system_prompt.is_empty() {
            line.push_str(&format!("\n  system_prompt: {system_prompt}"));
        }
        let workers: Vec<String> = stage
            .worker_agents
            .iter()
            .filter_map(|worker| {
                let id = worker.id.trim();
                let direction = worker.direction.trim();
                let prompt = worker.system_prompt.trim();
                if id.is_empty() && direction.is_empty() && prompt.is_empty() {
                    None
                } else {
                    Some(format!(
                        "{} [{}] prompt: {}",
                        id.if_empty("worker"),
                        direction.if_empty("general"),
                        prompt
                    ))
                }
            })
            .collect();
        if !workers.is_empty() {
            line.push_str(&format!("\n  workers:\n  - {}", workers.join("\n  - ")));
        }
        lines.push(line);
    }
    format!("Pipeline contract:\n{}", lines.join("\n"))
}

trait IfEmpty {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl IfEmpty for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

pub(crate) fn task_resume_hint(
    task: &super::store::TaskContext,
    user_prompt: &str,
    answer: &str,
) -> String {
    let mut parts = Vec::new();
    if !task.current_step.trim().is_empty() {
        parts.push(format!("step={}", task.current_step.trim()));
    }
    if !task.expected_action.trim().is_empty() {
        parts.push(format!("expected={}", task.expected_action.trim()));
    }
    if !task.goal.trim().is_empty() {
        parts.push(format!("goal={}", task.goal.trim()));
    }
    let prompt = user_prompt.trim();
    if !prompt.is_empty() {
        parts.push(format!(
            "last_user={}",
            prompt.chars().take(160).collect::<String>()
        ));
    }
    let answer = answer.trim();
    if !answer.is_empty() {
        parts.push(format!(
            "last_answer={}",
            answer.chars().take(160).collect::<String>()
        ));
    }
    parts.join("; ")
}

/// Render the long-term profile block: filled fields plus an explicit instruction
/// to interview the user for any missing required fields.
pub(crate) fn render_profile_block(profile: &super::store::AgentProfile) -> Option<String> {
    let filled: Vec<String> = profile
        .fields
        .iter()
        .filter(|field| field.is_filled())
        .map(|field| format!("- {}: {}", field.key, field.value.trim()))
        .collect();
    let pending: Vec<String> = profile
        .pending_required()
        .into_iter()
        .map(|field| {
            let question = field.question.trim();
            if question.is_empty() {
                format!("- {}", field.key)
            } else {
                format!("- {} ({})", question, field.key)
            }
        })
        .collect();
    if filled.is_empty() && pending.is_empty() {
        return None;
    }
    let mut block = String::from("[memory:long-term] User profile");
    if !filled.is_empty() {
        block.push_str(&format!(" (known):\n{}", filled.join("\n")));
    }
    if !pending.is_empty() {
        block.push_str(&format!(
            "\nStill missing — ask the user about these before going deep (one or two at a time, naturally):\n{}",
            pending.join("\n")
        ));
    }
    Some(block)
}

pub(crate) fn render_user_profile_block(
    profile: &super::store::UserProfile,
    seen_context: &[String],
) -> Option<String> {
    let mut lines = Vec::new();
    let name = profile.display_name.trim();
    if !name.is_empty() && !context_value_seen(seen_context, name) {
        lines.push(format!("Profile: {name}"));
    }
    push_profile_list(
        &mut lines,
        "Style preferences",
        &profile.style_preferences,
        seen_context,
    );
    push_profile_list(
        &mut lines,
        "Format preferences",
        &profile.format_preferences,
        seen_context,
    );
    push_profile_list(
        &mut lines,
        "User constraints",
        &profile.constraints,
        seen_context,
    );
    push_profile_list(
        &mut lines,
        "Language preferences",
        &profile.language_preferences,
        seen_context,
    );
    let response_length = profile.response_length.trim();
    if !response_length.is_empty() && !context_value_seen(seen_context, response_length) {
        lines.push(format!("Response length: {response_length}"));
    }
    let custom = profile.custom_instructions.trim();
    if !custom.is_empty() && !context_value_seen(seen_context, custom) {
        lines.push(format!("Custom instructions: {custom}"));
    }
    if lines.is_empty() {
        return None;
    }
    lines.push(
        "Scope: apply this profile to style, format, language, answer constraints, and user preferences only. Do not change the agent identity, tools, workflow, or capabilities. If the current user request explicitly conflicts with this profile, follow the current request."
            .to_string(),
    );
    Some(format!(
        "[user-profile] Runtime user profile preferences:\n{}",
        lines.join("\n")
    ))
}

fn push_profile_list(
    lines: &mut Vec<String>,
    label: &str,
    values: &[String],
    seen_context: &[String],
) {
    let values: Vec<&str> = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty() && !context_value_seen(seen_context, value))
        .collect();
    if !values.is_empty() {
        lines.push(format!("{label}:\n- {}", values.join("\n- ")));
    }
}

pub(crate) fn remember_agent_profile_context(
    seen: &mut Vec<String>,
    profile: &super::store::AgentProfile,
) {
    for field in profile.fields.iter().filter(|field| field.is_filled()) {
        remember_context_value(seen, &field.value);
        remember_context_value(seen, &format!("{}: {}", field.key, field.value.trim()));
    }
}

pub(crate) fn remember_context_value(seen: &mut Vec<String>, value: &str) {
    let normalized = normalize_context_value(value);
    if !normalized.is_empty() && !seen.iter().any(|item| item == &normalized) {
        seen.push(normalized);
    }
}

fn context_value_seen(seen: &[String], value: &str) -> bool {
    let normalized = normalize_context_value(value);
    !normalized.is_empty() && seen.iter().any(|item| item == &normalized)
}

fn normalize_context_value(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['-', '*', ':', ';', '.', ',', ' '])
        .to_lowercase()
}

fn fact_value_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(fact_value_to_string)
                .collect::<Vec<_>>()
                .join("; ");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

pub(crate) fn merge_optional_metrics(
    current: Option<RequestMetrics>,
    next: Option<RequestMetrics>,
) -> Option<RequestMetrics> {
    match (current, next) {
        (Some(current), Some(next)) => Some(crate::chat::add_request_metrics(&current, &next)),
        (Some(current), None) => Some(current),
        (None, Some(next)) => Some(next),
        (None, None) => None,
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
