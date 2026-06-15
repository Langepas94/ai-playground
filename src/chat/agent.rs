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
    format_messages_for_summary,
};
use super::store::TopicFileStorage;
use super::token_accounting::{TokenEstimate, estimate_exchange, estimate_messages_tokens};
use tokio::time::{Duration, timeout};

pub const LOCAL_SESSION_AGENT_ID: &str = "local-session-agent";
const MEMORY_COMPACT_TIMEOUT: Duration = Duration::from_secs(30);
const MEMORY_FACTS_EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
const TOPIC_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(20);
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
    working_context: Option<super::store::TaskContext>,
    knowledge: Option<String>,
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
            working_context: None,
            knowledge: None,
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

    /// Working-memory layer for the current task. Injected into each request as
    /// a tagged system block; not written into history/sidecar.
    pub fn set_working_context(&mut self, task: Option<super::store::TaskContext>) {
        self.working_context = task.filter(|task| !task.is_empty());
    }

    /// Long-term knowledge document. Injected into each request as a tagged
    /// system block; not written into history/sidecar.
    pub fn set_knowledge(&mut self, knowledge: Option<String>) {
        self.knowledge = knowledge
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
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

    /// Insert the explicit working/long-term memory blocks after the leading
    /// system messages, so the provider sees task + knowledge context up front.
    fn inject_memory_layers(&self, messages: &mut Vec<ChatMessage>) {
        let mut blocks = Vec::new();
        if let Some(task) = self.working_context.as_ref() {
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!("[memory:working] Task context:\n{}", render_task(task)),
            });
        }
        if let Some(knowledge) = self.knowledge.as_deref() {
            blocks.push(ChatMessage {
                role: Role::System,
                content: format!("[memory:long-term] Knowledge:\n{knowledge}"),
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

/// Render a `TaskContext` into a compact, readable block for the provider.
fn render_task(task: &super::store::TaskContext) -> String {
    let mut lines = Vec::new();
    for (label, value) in [
        ("Title", task.title.as_str()),
        ("Goal", task.goal.as_str()),
        ("Status", task.status.as_str()),
    ] {
        let value = value.trim();
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    let steps: Vec<&str> = task
        .steps
        .iter()
        .map(|step| step.trim())
        .filter(|step| !step.is_empty())
        .collect();
    if !steps.is_empty() {
        lines.push(format!("Steps:\n- {}", steps.join("\n- ")));
    }
    let notes = task.notes.trim();
    if !notes.is_empty() {
        lines.push(format!("Notes: {notes}"));
    }
    lines.join("\n")
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
