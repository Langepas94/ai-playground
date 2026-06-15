use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, RequestMetrics, ResponseControl, Role,
    },
};

use super::memory::{
    AgentMemory, MemoryConfig, MemoryLayer, MemoryStrategy, TopicRouteDecision,
    format_messages_for_summary, looks_sensitive,
};
use super::store::{ProfileField, TaskStage, TopicFileStorage};
use super::token_accounting::{TokenEstimate, estimate_exchange, estimate_messages_tokens};
use tokio::time::{Duration, timeout};

pub const LOCAL_SESSION_AGENT_ID: &str = "local-session-agent";
const MEMORY_COMPACT_TIMEOUT: Duration = Duration::from_secs(30);
const MEMORY_FACTS_EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
const TOPIC_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(20);
const STATEFUL_STEP_TIMEOUT: Duration = Duration::from_secs(20);
const PREFLIGHT_COMPACT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PREFLIGHT_SUMMARY_PASSES: usize = 3;

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
enum PromptIngest {
    Continue(Option<RequestMetrics>),
    TopicNotFound {
        metrics: Option<RequestMetrics>,
        message: String,
    },
}

impl PromptIngest {
    fn into_parts(self) -> (Option<RequestMetrics>, Option<String>) {
        match self {
            Self::Continue(metrics) => (metrics, None),
            Self::TopicNotFound { metrics, message } => (metrics, Some(message)),
        }
    }
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
        }
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

    /// Free-text domain description that keeps the agent on-topic.
    pub fn set_domain(&mut self, domain: impl Into<String>) {
        self.domain = domain.into().trim().to_string();
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

    /// Record user prompt + assistant response into history after streaming completes.
    pub fn record_stream_response(&mut self, prompt: String, answer: String) {
        self.commit_turn(prompt.clone(), answer.clone());
        self.apply_context_storage_policy();
        let _ = self.update_active_topic_file(&prompt, &answer);
    }

    pub async fn compact_memory(
        &mut self,
        client: &dyn ProviderClient,
    ) -> Option<crate::providers::RequestMetrics> {
        self.refresh_memory_with_timeout(client).await
    }

    pub async fn respond(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<ChatResponse, AppError> {
        let (mut context_metrics, topic_not_found) =
            self.ingest_user_prompt(client, &prompt).await?.into_parts();
        if let Some(message) = topic_not_found {
            self.commit_turn(prompt, message.clone());
            return Ok(local_chat_response(message, context_metrics));
        }
        context_metrics = merge_optional_metrics(
            context_metrics,
            self.precompact_before_request(client, &prompt).await,
        );
        let response = client
            .chat_completion(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt.clone(), response.text.clone());
        self.apply_context_storage_policy();
        self.update_active_topic_file(prompt.as_str(), response.text.as_str())?;
        let mut response = response;
        if let Some(metrics) = context_metrics {
            response.metrics = crate::chat::add_request_metrics(&response.metrics, &metrics);
        }
        if let Some(metrics) = self.refresh_memory_with_timeout(client).await {
            response.metrics = crate::chat::add_request_metrics(&response.metrics, &metrics);
        }
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
        let (mut context_metrics, topic_not_found) =
            self.ingest_user_prompt(client, &prompt).await?.into_parts();
        if let Some(message) = topic_not_found {
            self.commit_turn(prompt, message.clone());
            let response = local_chat_response(message, context_metrics.clone());
            return Ok((response, local_provider_debug(), context_metrics));
        }
        context_metrics = merge_optional_metrics(
            context_metrics,
            self.precompact_before_request(client, &prompt).await,
        );
        let (response, debug) = client
            .chat_completion_with_debug(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt.clone(), response.text.clone());
        self.apply_context_storage_policy();
        self.update_active_topic_file(prompt.as_str(), response.text.as_str())?;
        let mut response = response;
        if let Some(metrics) = &context_metrics {
            response.metrics = crate::chat::add_request_metrics(&response.metrics, metrics);
        }
        if let Some(metrics) = self.refresh_memory_with_timeout(client).await {
            context_metrics = Some(match context_metrics {
                Some(current) => crate::chat::add_request_metrics(&current, &metrics),
                None => metrics.clone(),
            });
            response.metrics = crate::chat::add_request_metrics(&response.metrics, &metrics);
        }
        Ok((response, debug, context_metrics))
    }

    pub async fn prepare_stream_request(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Result<PreparedStreamRequest, AppError> {
        let (mut context_metrics, topic_not_found) = self
            .ingest_user_prompt(client, prompt)
            .await
            .map(PromptIngest::into_parts)?;
        if let Some(message) = topic_not_found {
            self.commit_turn(prompt.to_string(), message.clone());
            return Ok(PreparedStreamRequest {
                request: None,
                context_metrics: context_metrics.clone(),
                local_response: Some(local_chat_response(message, context_metrics)),
            });
        }
        context_metrics = merge_optional_metrics(
            context_metrics,
            self.precompact_before_request(client, prompt).await,
        );
        Ok(PreparedStreamRequest {
            request: Some(self.request_with_user_prompt(prompt.to_string())),
            context_metrics,
            local_response: None,
        })
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

        if !self.domain.is_empty() {
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!("[agent:domain] {}", self.domain),
            });
        }

        if let Some(profile) = self.agent_profile.as_ref()
            && let Some(block) = render_profile_block(profile)
        {
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
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!(
                    "[invariants] These constraints are absolute and must never be broken, even if the user asks:\n- {}",
                    self.invariants.join("\n- ")
                ),
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

    async fn ingest_user_prompt(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Result<PromptIngest, AppError> {
        match self.memory_config.strategy {
            MemoryStrategy::StickyFacts => {
                return Ok(PromptIngest::Continue(
                    self.update_sticky_facts(client, prompt).await,
                ));
            }
            MemoryStrategy::ScopedBranches if self.memory_config.topic_file_routing => {
                return self.route_topic_file(client, prompt).await;
            }
            MemoryStrategy::ScopedBranches if self.memory_config.scoped_auto_route => {
                let branch = self.memory.select_scoped_topic(
                    prompt,
                    &self.history,
                    &self.memory_config.active_branch,
                );
                self.memory_config.active_branch = branch;
            }
            _ => {}
        }
        Ok(PromptIngest::Continue(None))
    }

    async fn route_topic_file(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Result<PromptIngest, AppError> {
        self.memory
            .ensure_topic_catalog_from_branches(&self.history, &self.memory_config.active_branch);
        if self.memory.topic_catalog.is_empty() {
            if self.memory_config.topic_auto_create {
                let topic_id = self.memory.select_scoped_topic(
                    prompt,
                    &self.history,
                    &self.memory_config.active_branch,
                );
                self.activate_topic_file(topic_id.as_str(), prompt)?;
                return Ok(PromptIngest::Continue(None));
            }
            return Ok(PromptIngest::TopicNotFound {
                metrics: None,
                message: topic_not_found_message("topic catalog is empty"),
            });
        }

        let classifier_prompt = self.memory_config.topic_classifier_prompt.trim();
        let classifier_prompt = if classifier_prompt.is_empty() {
            super::memory::DEFAULT_TOPIC_CLASSIFIER_PROMPT
        } else {
            classifier_prompt
        };
        let request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: classifier_prompt.to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!(
                        "{}\n\nLatest user message:\n{prompt}\n\nReturn the JSON route decision only.",
                        self.memory.compact_topic_catalog()
                    ),
                },
            ],
            control: topic_classifier_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let result = timeout(
            TOPIC_CLASSIFIER_TIMEOUT,
            client.chat_completion(&self.profile, &self.token, request),
        )
        .await;
        let Ok(Ok(response)) = result else {
            return Ok(PromptIngest::TopicNotFound {
                metrics: None,
                message: topic_not_found_message("classifier did not return a route"),
            });
        };
        let metrics = Some(response.metrics);
        let decision =
            parse_topic_route_decision(response.text.as_str()).unwrap_or(TopicRouteDecision {
                found: false,
                topic_id: None,
                confidence: 0.0,
                reason: "classifier response was not valid JSON".to_string(),
            });
        self.memory.last_topic_route = Some(decision.clone());
        if decision.found
            && let Some(topic_id) = decision.topic_id.as_deref()
            && self.memory.topic_catalog.contains_key(topic_id)
        {
            self.activate_topic_file(topic_id, prompt)?;
            return Ok(PromptIngest::Continue(metrics));
        }

        if self.memory_config.topic_auto_create {
            let topic_id = self.memory.select_scoped_topic(
                prompt,
                &self.history,
                &self.memory_config.active_branch,
            );
            self.activate_topic_file(topic_id.as_str(), prompt)?;
            return Ok(PromptIngest::Continue(metrics));
        }

        Ok(PromptIngest::TopicNotFound {
            metrics,
            message: topic_not_found_message(&decision.reason),
        })
    }

    fn activate_topic_file(&mut self, topic_id: &str, prompt: &str) -> Result<(), AppError> {
        let topic_id = topic_id.trim();
        if topic_id.is_empty() {
            return Ok(());
        }
        self.memory_config.active_branch = topic_id.to_string();
        let loaded = self
            .topic_store
            .as_ref()
            .map(|store| store.load_topic_file(topic_id))
            .transpose()?
            .flatten();
        let topic_file = loaded.unwrap_or_else(|| {
            let config = self.memory_config.clone();
            self.memory
                .topic_file_from_branch_history(topic_id, prompt, &self.history, &config)
        });
        self.memory.active_topic_file = Some(topic_file);
        Ok(())
    }

    fn update_active_topic_file(&mut self, prompt: &str, answer: &str) -> Result<(), AppError> {
        if self.memory_config.strategy != MemoryStrategy::ScopedBranches
            || !self.memory_config.topic_file_routing
        {
            return Ok(());
        }
        self.memory.update_active_topic_file(prompt, answer);
        if let (Some(store), Some(topic_file)) = (
            self.topic_store.as_ref(),
            self.memory.active_topic_file.as_ref(),
        ) {
            store.save_topic_file(topic_file)?;
        }
        Ok(())
    }

    async fn update_sticky_facts(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Option<RequestMetrics> {
        let extraction_prompt = self.memory_config.facts_extraction_prompt.trim();
        if extraction_prompt.is_empty() {
            self.memory.update_facts_from_user_message(prompt);
            return None;
        }
        let existing_facts =
            serde_json::to_string(&self.memory.facts).unwrap_or_else(|_| "{}".to_string());
        let request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: extraction_prompt.to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!(
                        "Existing facts JSON:\n{existing_facts}\n\nLatest user message:\n{prompt}\n\nReturn JSON object with fact updates only."
                    ),
                },
            ],
            control: memory_facts_extraction_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let result = timeout(
            MEMORY_FACTS_EXTRACT_TIMEOUT,
            client.chat_completion(&self.profile, &self.token, request),
        )
        .await;
        let Ok(Ok(response)) = result else {
            self.memory.update_facts_from_user_message(prompt);
            return None;
        };
        if let Some(facts) = parse_extracted_facts(response.text.as_str()) {
            if facts.is_empty() {
                return Some(response.metrics);
            }
            self.memory.merge_extracted_facts_with_layers(
                facts
                    .into_iter()
                    .map(|fact| (fact.key, fact.value, fact.layer)),
            );
        } else {
            self.memory.update_facts_from_user_message(prompt);
        }
        Some(response.metrics)
    }

    /// Run the stateful post-processing for one completed turn: fill profile
    /// fields from the user's message, advance the task-stage FSM, and check the
    /// response against the invariants. Each step is gated on the relevant data
    /// being configured, so an ad-hoc (non-agent) chat pays nothing here. Returns
    /// a report for the debug view. Internal state (profile, task stage,
    /// violations) is mutated in place; the web layer persists it afterwards.
    pub async fn stateful_postprocess(
        &mut self,
        client: &dyn ProviderClient,
        user_prompt: &str,
        answer: &str,
    ) -> StatefulReport {
        let mut report = StatefulReport::default();

        if self.agent_profile.is_some()
            && let Some(metrics) = self.fill_profile_from_message(client, user_prompt).await
        {
            report.metrics = Some(metrics);
        }
        if let Some(profile) = self.agent_profile.as_ref() {
            report.pending_questions = profile
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

        if self.task_state.is_some() {
            let (transition, metrics) = self.advance_task_stage(client, user_prompt, answer).await;
            report.stage = self.task_state.as_ref().map(|task| task.stage);
            report.stage_transition = transition;
            report.metrics = merge_optional_metrics(report.metrics, metrics);
        }

        if !self.invariants.is_empty() {
            let (violations, metrics) = self.check_invariants(client, answer).await;
            report.violations = violations.clone();
            report.metrics = merge_optional_metrics(report.metrics, metrics);
            if let Some(task) = self.task_state.as_mut() {
                task.violations = violations;
            }
        }

        report
    }

    /// LLM extraction guided by the profile schema: map the user's latest message
    /// onto known field keys and store any values it reveals.
    async fn fill_profile_from_message(
        &mut self,
        client: &dyn ProviderClient,
        user_prompt: &str,
    ) -> Option<RequestMetrics> {
        let profile = self.agent_profile.as_ref()?;
        if profile.fields.is_empty() {
            return None;
        }
        let schema = profile
            .fields
            .iter()
            .map(|field| format!("- {} (question: {})", field.key, field.question.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: "You extract profile field values from a user message. Only return values the user actually stated. Reply with a JSON object mapping field keys to string values; omit fields not mentioned. No prose.".to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!(
                        "Profile fields:\n{schema}\n\nUser message:\n{user_prompt}\n\nReturn JSON of {{key: value}} for fields the user just provided."
                    ),
                },
            ],
            control: memory_facts_extraction_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let result = timeout(
            STATEFUL_STEP_TIMEOUT,
            client.chat_completion(&self.profile, &self.token, request),
        )
        .await;
        let Ok(Ok(response)) = result else {
            return None;
        };
        let updates = parse_profile_updates(response.text.as_str());
        if let Some(profile) = self.agent_profile.as_mut() {
            let mut changed = false;
            for (key, value) in updates {
                if value.trim().is_empty() || looks_sensitive(&value) {
                    continue;
                }
                if let Some(field) = profile.fields.iter_mut().find(|field| field.key == key) {
                    field.value = value.trim().to_string();
                    changed = true;
                }
            }
            if changed {
                profile.updated_at_unix = crate::chat::unix_now();
            }
        }
        Some(response.metrics)
    }

    /// Ask the model which stage best fits the dialog now, then apply it only if
    /// the task-state FSM allows the transition. Illegal proposals are rejected.
    async fn advance_task_stage(
        &mut self,
        client: &dyn ProviderClient,
        user_prompt: &str,
        answer: &str,
    ) -> (Option<StageTransition>, Option<RequestMetrics>) {
        let current = match self.task_state.as_ref() {
            Some(task) => task.stage,
            None => return (None, None),
        };
        let allowed = current.allowed_next();
        if allowed.is_empty() {
            return (None, None);
        }
        let options = TaskStage::ORDERED
            .iter()
            .map(TaskStage::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: format!(
                        "You track a task's stage. Stages: {options}. Given the current stage and the latest exchange, decide the stage the work is in NOW. Reply with JSON {{\"stage\":\"<stage>\",\"reason\":\"...\"}}. Prefer staying in the current stage unless the exchange clearly moved it forward or back."
                    ),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!(
                        "Current stage: {current}\n\nUser:\n{user_prompt}\n\nAssistant:\n{answer}"
                    ),
                },
            ],
            control: topic_classifier_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let result = timeout(
            STATEFUL_STEP_TIMEOUT,
            client.chat_completion(&self.profile, &self.token, request),
        )
        .await;
        let Ok(Ok(response)) = result else {
            return (None, None);
        };
        let Some(proposed) = parse_proposed_stage(response.text.as_str()) else {
            return (None, Some(response.metrics));
        };
        if proposed == current {
            return (None, Some(response.metrics));
        }
        let accepted = current.can_transition(proposed);
        if accepted && let Some(task) = self.task_state.as_mut() {
            task.stage = proposed;
        }
        (
            Some(StageTransition {
                from: current,
                to: proposed,
                accepted,
            }),
            Some(response.metrics),
        )
    }

    /// Validate the response against the invariants in code (via a cheap LLM
    /// check). Returns the list of violated invariants (empty = clean).
    async fn check_invariants(
        &self,
        client: &dyn ProviderClient,
        answer: &str,
    ) -> (Vec<String>, Option<RequestMetrics>) {
        if self.invariants.is_empty() {
            return (Vec::new(), None);
        }
        let list = self
            .invariants
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}. {line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: "You are a strict invariant checker. Return ONLY constraints the assistant response CLEARLY and CONCRETELY breaks — e.g. it recommends or produces something a constraint forbids. Asking questions, clarifying, planning, or simply not mentioning a constraint is NOT a violation. When in doubt, return nothing. Reply with JSON {\"violations\":[\"<exact constraint text>\", ...]}; empty array if the response is fine. No prose.".to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!("Constraints:\n{list}\n\nAssistant response:\n{answer}\n\nWhich constraints (if any) does this response actually violate?"),
                },
            ],
            control: topic_classifier_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let result = timeout(
            STATEFUL_STEP_TIMEOUT,
            client.chat_completion(&self.profile, &self.token, request),
        )
        .await;
        let Ok(Ok(response)) = result else {
            return (Vec::new(), None);
        };
        (
            parse_invariant_violations(response.text.as_str()),
            Some(response.metrics),
        )
    }

    fn apply_context_storage_policy(&mut self) {
        self.memory
            .apply_scoped_branch_storage_policy(&mut self.history, &self.memory_config);
    }

    async fn refresh_memory_with_timeout(
        &mut self,
        client: &dyn ProviderClient,
    ) -> Option<crate::providers::RequestMetrics> {
        timeout(MEMORY_COMPACT_TIMEOUT, self.refresh_memory(client))
            .await
            .ok()
            .flatten()
    }

    async fn refresh_memory(
        &mut self,
        client: &dyn ProviderClient,
    ) -> Option<crate::providers::RequestMetrics> {
        let range = self
            .memory
            .next_summary_range(&self.history, &self.memory_config)?;
        self.compact_history_range(client, range).await
    }

    async fn precompact_before_request(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Option<crate::providers::RequestMetrics> {
        if self.memory_config.strategy != MemoryStrategy::Summary {
            return None;
        }
        timeout(
            PREFLIGHT_COMPACT_TIMEOUT,
            self.precompact_for_context_pressure(client, prompt),
        )
        .await
        .ok()
        .flatten()
    }

    async fn precompact_for_context_pressure(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Option<crate::providers::RequestMetrics> {
        let threshold = self.preflight_summary_threshold()?;
        let mut accumulated = None;
        let mut previous_tokens = None;

        for _ in 0..MAX_PREFLIGHT_SUMMARY_PASSES {
            let request = self.request_with_user_prompt(prompt.to_string());
            let request_tokens = estimate_messages_tokens(&request.messages);
            if request_tokens < threshold {
                break;
            }
            if previous_tokens.is_some_and(|tokens| request_tokens >= tokens) {
                break;
            }
            let range = self.memory.next_summary_range_for_pressure(
                &self.history,
                &self.memory_config,
                self.memory_config.recent_messages,
            )?;
            let metrics = self.compact_history_range(client, range).await?;
            accumulated = Some(match accumulated {
                Some(current) => crate::chat::add_request_metrics(&current, &metrics),
                None => metrics,
            });
            previous_tokens = Some(request_tokens);
        }

        accumulated
    }

    fn preflight_summary_threshold(&self) -> Option<u32> {
        let context_limit = self.context_limit?;
        let reserved_output = self
            .control
            .max_completion_tokens
            .or(self.control.max_tokens)
            .unwrap_or(0);
        let available_input = context_limit.saturating_sub(reserved_output);
        if available_input == 0 {
            return None;
        }
        Some(
            available_input
                .saturating_mul(u32::from(self.memory_config.summarize_at_context_percent))
                / 100,
        )
    }

    async fn compact_history_range(
        &mut self,
        client: &dyn ProviderClient,
        range: std::ops::Range<usize>,
    ) -> Option<crate::providers::RequestMetrics> {
        let messages_to_summarize = format_messages_for_summary(&self.history[range.clone()]);
        if messages_to_summarize.trim().is_empty() {
            self.memory.summarized_message_count = range.end;
            return None;
        }
        let previous_summary = self
            .memory
            .session_summary
            .as_deref()
            .unwrap_or("No previous summary.");
        let summary_prompt = self.memory_config.summary_prompt.trim();
        let summary_prompt = if summary_prompt.is_empty() {
            super::memory::DEFAULT_SUMMARY_PROMPT
        } else {
            summary_prompt
        };
        let summary_request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: summary_prompt.to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: format!(
                        "Previous memory summary:\n{previous_summary}\n\nNew chat fragment to merge:\n{messages_to_summarize}\n\nReturn the updated memory summary."
                    ),
                },
            ],
            control: memory_summary_control(),
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        if let Ok(response) = client
            .chat_completion(&self.profile, &self.token, summary_request)
            .await
        {
            let summary = response.text.trim();
            if !summary.is_empty() {
                self.memory.session_summary = Some(summary.to_string());
                self.memory.summarized_message_count = range.end;
                return Some(response.metrics);
            }
        }
        None
    }
}

fn memory_summary_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.2);
    control.max_tokens = Some(700);
    control
}

fn memory_facts_extraction_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.0);
    control.max_tokens = Some(500);
    control
}

fn topic_classifier_control() -> ResponseControl {
    let mut control = ResponseControl::uncontrolled();
    control.temperature = Some(0.0);
    control.max_tokens = Some(250);
    control
}

fn parse_topic_route_decision(text: &str) -> Option<TopicRouteDecision> {
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

fn topic_not_found_message(reason: &str) -> String {
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

fn local_chat_response(text: String, metrics: Option<RequestMetrics>) -> ChatResponse {
    ChatResponse {
        text,
        finish_reason: Some("topic_not_found".to_string()),
        metrics: metrics.unwrap_or_default(),
    }
}

fn local_provider_debug() -> ProviderExchangeDebug {
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
fn parse_extracted_facts(text: &str) -> Option<Vec<ExtractedFact>> {
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
    /// The transition decided this turn, if any (includes rejected ones).
    pub stage_transition: Option<StageTransition>,
    /// Invariants the last response violated (empty = clean).
    pub violations: Vec<String>,
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
fn parse_profile_updates(text: &str) -> Vec<(String, String)> {
    let Some(serde_json::Value::Object(map)) = extract_json_value(text) else {
        return Vec::new();
    };
    map.into_iter()
        .filter_map(|(key, value)| fact_value_to_string(&value).map(|value| (key, value)))
        .collect()
}

/// Parse `{"stage":"...","reason":"..."}` into a stage, if recognised.
fn parse_proposed_stage(text: &str) -> Option<TaskStage> {
    let value = extract_json_value(text)?;
    let stage = value.as_object()?.get("stage")?.as_str()?;
    stage.parse().ok()
}

/// Parse `{"violations":[...]}` into a list of violated-constraint strings.
fn parse_invariant_violations(text: &str) -> Vec<String> {
    let Some(value) = extract_json_value(text) else {
        return Vec::new();
    };
    let array = match &value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map
            .get("violations")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    array
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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

/// Render the working-memory task block: stage FSM + plan, with the allowed next
/// stages and a hard no-skip instruction.
fn render_task_block(task: &super::store::TaskContext) -> String {
    let mut lines = vec![format!("Stage: {}", task.stage)];
    let allowed = task.stage.allowed_next();
    if allowed.is_empty() {
        lines.push("This task is done; do not reopen stages.".to_string());
    } else {
        let names: Vec<String> = allowed.iter().map(|stage| stage.to_string()).collect();
        lines.push(format!(
            "Allowed next stages: {}. Do NOT skip stages even if the user asks.",
            names.join(", ")
        ));
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

/// Render the long-term profile block: filled fields plus an explicit instruction
/// to interview the user for any missing required fields.
fn render_profile_block(profile: &super::store::AgentProfile) -> Option<String> {
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

fn merge_optional_metrics(
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
