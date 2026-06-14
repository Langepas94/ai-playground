use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, ResponseControl, Role,
    },
};

use super::memory::{AgentMemory, MemoryConfig, MemoryStrategy, format_messages_for_summary};
use super::token_accounting::{TokenEstimate, estimate_exchange, estimate_messages_tokens};
use tokio::time::{Duration, timeout};

pub const LOCAL_SESSION_AGENT_ID: &str = "local-session-agent";
const MEMORY_COMPACT_TIMEOUT: Duration = Duration::from_secs(30);
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
        }
    }

    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    pub fn memory(&self) -> &AgentMemory {
        &self.memory
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
        self.commit_turn(prompt, answer);
        self.apply_context_storage_policy();
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
        self.ingest_user_prompt(&prompt);
        let context_metrics = self.precompact_before_request(client, &prompt).await;
        let response = client
            .chat_completion(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        self.apply_context_storage_policy();
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
        self.ingest_user_prompt(&prompt);
        let mut context_metrics = self.precompact_before_request(client, &prompt).await;
        let (response, debug) = client
            .chat_completion_with_debug(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        self.apply_context_storage_policy();
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
    ) -> (
        crate::providers::ChatRequest,
        Option<crate::providers::RequestMetrics>,
    ) {
        self.ingest_user_prompt(prompt);
        let context_metrics = self.precompact_before_request(client, prompt).await;
        (
            self.request_with_user_prompt(prompt.to_string()),
            context_metrics,
        )
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

    fn request_with_user_prompt(&self, prompt: String) -> ChatRequest {
        let mut memory_config = self.memory_config.clone();
        if memory_config.strategy == MemoryStrategy::StickyFacts {
            // The current user prompt is part of "last N messages", so only
            // N-1 previous non-system messages are pulled from history here.
            memory_config.recent_messages = memory_config.recent_messages.saturating_sub(1);
        }
        let mut messages = self.memory.build_context(&self.history, &memory_config);
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

    fn ingest_user_prompt(&mut self, prompt: &str) {
        match self.memory_config.strategy {
            MemoryStrategy::StickyFacts => {
                self.memory.update_facts_from_user_message(prompt);
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Debug, Default)]
    struct FakeClient {
        replies: std::sync::Mutex<Vec<String>>,
        metrics: std::sync::Mutex<Vec<crate::providers::RequestMetrics>>,
        seen_messages: std::sync::Mutex<Vec<Vec<ChatMessage>>>,
    }

    #[async_trait]
    impl ProviderClient for FakeClient {
        async fn list_models(
            &self,
            _profile: &ProfileConfig,
            _token: &str,
        ) -> Result<Vec<String>, AppError> {
            Ok(Vec::new())
        }

        async fn chat_completion(
            &self,
            _profile: &ProfileConfig,
            _token: &str,
            request: ChatRequest,
        ) -> Result<ChatResponse, AppError> {
            self.seen_messages
                .lock()
                .expect("seen messages")
                .push(request.messages);
            let text = self
                .replies
                .lock()
                .expect("replies")
                .pop()
                .unwrap_or_else(|| "ok".to_string());
            let metrics = self.metrics.lock().expect("metrics").pop().unwrap_or(
                crate::providers::RequestMetrics {
                    elapsed_ms: 1,
                    usage: None,
                    cost: None,
                },
            );
            Ok(ChatResponse {
                text,
                finish_reason: Some("stop".to_string()),
                metrics,
            })
        }

        async fn chat_completion_with_debug(
            &self,
            profile: &ProfileConfig,
            token: &str,
            request: ChatRequest,
        ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
            let response = self.chat_completion(profile, token, request).await?;
            Ok((
                response,
                ProviderExchangeDebug {
                    request: crate::providers::HttpDebugRequest {
                        method: "POST".to_string(),
                        url: "https://example.test/v1/chat/completions".to_string(),
                        headers: Default::default(),
                        body: serde_json::json!({}),
                    },
                    response: crate::providers::HttpDebugResponse {
                        status: 200,
                        headers: Default::default(),
                        body: serde_json::json!({}),
                    },
                },
            ))
        }
    }

    fn test_profile() -> ProfileConfig {
        ProfileConfig {
            provider: crate::providers::ProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            base_url: "https://example.test/v1".to_string(),
            token_ref: "openai-compatible".to_string(),
        }
    }

    #[tokio::test]
    async fn sliding_window_sends_only_recent_messages_and_prunes_history() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = (0..6)
            .map(|index| ChatMessage {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("history {index}"),
            })
            .collect::<Vec<_>>();
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: 2,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].len(), 3);
        assert_eq!(seen[0][0].content, "history 4");
        assert_eq!(seen[0][1].content, "history 5");
        assert_eq!(seen[0][2].content, "current question");
        assert_eq!(agent.history().len(), 2);
        assert_eq!(agent.history()[0].content, "current question");
        assert_eq!(agent.history()[1].content, "answer");
    }

    #[tokio::test]
    async fn sticky_facts_updates_before_request_and_sends_facts_block() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            vec![ChatMessage {
                role: Role::System,
                content: "Base system".to_string(),
            }],
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 4,
            ..MemoryConfig::default()
        });

        agent
            .respond(
                &client,
                "цель: реализовать context strategies\nОтвечай кратко".to_string(),
            )
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen[0].len(), 3);
        assert_eq!(seen[0][0].role, Role::System);
        assert_eq!(seen[0][0].content, "Base system");
        assert_eq!(seen[0][1].role, Role::System);
        assert!(seen[0][1].content.contains("FACTS_KV:"));
        assert!(
            seen[0][1]
                .content
                .contains("цель: реализовать context strategies")
        );
        assert!(seen[0][1].content.contains("preferences: Отвечай кратко"));
        assert!(agent.memory().facts.contains_key("цель"));
    }

    #[tokio::test]
    async fn sticky_facts_uses_facts_plus_window_without_extra_provider_call() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = (0..6)
            .map(|index| ChatMessage {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("history {index}"),
            })
            .collect::<Vec<_>>();
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 2,
            ..MemoryConfig::default()
        });

        agent
            .respond(
                &client,
                "goal: keep durable facts\napi key: sk-should-not-leak".to_string(),
            )
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(
            seen.len(),
            1,
            "facts extraction must be local, not a second provider request"
        );
        let request = &seen[0];
        assert_eq!(
            request.len(),
            3,
            "provider request must be facts + exactly last N raw messages including current prompt"
        );
        assert_eq!(request[0].role, Role::System);
        assert!(request[0].content.contains("FACTS_KV:"));
        assert!(request[0].content.contains("goal: keep durable facts"));
        assert!(!request[0].content.contains("sk-should-not-leak"));
        assert_eq!(request[1].content, "history 5");
        assert_eq!(
            request[2].content,
            "goal: keep durable facts\napi key: sk-should-not-leak"
        );
        assert!(
            request
                .iter()
                .all(|message| !message.content.contains("history 0"))
        );
        assert_eq!(
            agent.history().len(),
            8,
            "Sticky Facts must keep full local conversation history; recent_messages only limits provider request"
        );
        assert_eq!(agent.history()[0].content, "history 0");
        assert_eq!(agent.history()[5].content, "history 5");
        assert_eq!(
            agent.history()[6].content,
            "goal: keep durable facts\napi key: sk-should-not-leak"
        );
        assert_eq!(agent.history()[7].content, "answer");
    }

    #[tokio::test]
    async fn sticky_facts_update_after_every_user_message_and_request_sends_facts_plus_last_n() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "second answer".to_string(),
                "first answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            Vec::new(),
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 3,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "goal: build reliable facts memory".to_string())
            .await
            .expect("first response");
        agent
            .respond(
                &client,
                "constraint: show persisted KV and provider block".to_string(),
            )
            .await
            .expect("second response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0][0]
                .content
                .contains("goal: build reliable facts memory")
        );
        assert_eq!(
            seen[0]
                .iter()
                .filter(|message| message.role != Role::System)
                .count(),
            1,
            "first request has facts plus the current user message"
        );

        let second_request = &seen[1];
        let facts = &second_request[0].content;
        assert!(facts.contains("FACTS_KV:"));
        assert!(facts.contains("- goal: build reliable facts memory"));
        assert!(facts.contains("- constraint: show persisted KV and provider block"));
        let raw_messages = second_request
            .iter()
            .filter(|message| message.role != Role::System)
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            raw_messages,
            vec![
                "goal: build reliable facts memory",
                "first answer",
                "constraint: show persisted KV and provider block"
            ],
            "recent_messages=3 means the provider sees exactly the last 3 raw messages including current user prompt"
        );
        assert_eq!(
            agent.memory().facts.get("goal").map(String::as_str),
            Some("build reliable facts memory")
        );
        assert_eq!(
            agent.memory().facts.get("constraint").map(String::as_str),
            Some("show persisted KV and provider block")
        );
        assert_eq!(
            agent
                .history()
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec![
                "goal: build reliable facts memory",
                "first answer",
                "constraint: show persisted KV and provider block",
                "second answer"
            ],
            "Sticky Facts keeps full saved history while request uses only a recent window"
        );
    }

    #[tokio::test]
    async fn sticky_facts_sends_custom_facts_prompt_to_provider() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            Vec::new(),
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 2,
            facts_prompt: "Use these project facts when answering.".to_string(),
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "goal: expose custom facts prompt".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0][0]
                .content
                .starts_with("Use these project facts when answering.")
        );
        assert!(
            seen[0][0]
                .content
                .contains("goal: expose custom facts prompt")
        );
    }

    #[tokio::test]
    async fn summary_strategy_compacts_old_history_after_response() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "summary of older turns".to_string(),
                "fresh answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = (0..20)
            .map(|index| ChatMessage {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("history {index}"),
            })
            .collect::<Vec<_>>();
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::Summary,
            recent_messages: 4,
            summarize_after_messages: 18,
            summary_chunk_messages: 4,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].len(),
            21,
            "first request must not drop unsummarized history"
        );
        assert_eq!(seen[0][0].content, "history 0");
        assert_eq!(seen[0][20].content, "current question");
        assert_eq!(seen[1][0].role, Role::System);
        assert!(seen[1][0].content.contains("memory compaction module"));
        assert!(seen[1][1].content.contains("history 0"));
        assert_eq!(
            agent.memory().session_summary.as_deref(),
            Some("summary of older turns")
        );
        assert_eq!(agent.history().len(), 22);
    }

    #[tokio::test]
    async fn summary_strategy_sends_custom_summary_prompt_to_provider() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "custom summary".to_string(),
                "fresh answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = (0..8)
            .map(|index| ChatMessage {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("history {index}"),
            })
            .collect::<Vec<_>>();
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::Summary,
            recent_messages: 2,
            summarize_after_messages: 4,
            summary_chunk_messages: 2,
            summary_prompt: "CUSTOM SUMMARY PROMPT".to_string(),
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1][0].content, "CUSTOM SUMMARY PROMPT");
        assert!(seen[1][1].content.contains("Previous memory summary"));
        assert!(seen[1][1].content.contains("history 0"));
    }

    #[test]
    fn agent_estimates_next_exchange_from_current_history() {
        let agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            vec![
                ChatMessage {
                    role: Role::System,
                    content: "Ты эксперт по Civilization 6.".to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: "Как играть за Византию?".to_string(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: "Через религию и кавалерию.".to_string(),
                },
            ],
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            Some(ModelPricing {
                currency: "USD".to_string(),
                input_per_million: Some(1.0),
                output_per_million: 4.0,
                cache_hit_input_per_million: None,
                cache_miss_input_per_million: None,
            }),
            None,
        );

        let estimate = agent.estimate_next_exchange(
            "Что делать, если я отстал по науке?",
            "Сократи религиозные вложения и подними кампусы.",
            512,
        );

        assert!(estimate.current_request_tokens > estimate.history_tokens);
        assert!(estimate.response_tokens > 0);
        assert!(estimate.cost.expect("cost").amount > 0.0);
    }

    #[tokio::test]
    async fn branching_strategy_uses_current_branch_history_only() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["branch answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = vec![
            ChatMessage {
                role: Role::User,
                content: "checkpoint question".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "checkpoint answer".to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: "branch A turn".to_string(),
            },
        ];
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::Branching,
            recent_messages: 8,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "continue branch A".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0]
                .iter()
                .any(|message| message.content.contains("branch A turn"))
        );
        assert!(
            seen[0]
                .iter()
                .all(|message| !message.content.contains("branch B"))
        );
    }

    #[tokio::test]
    async fn branching_keeps_two_branches_independent_from_same_checkpoint() {
        let checkpoint = vec![
            ChatMessage {
                role: Role::User,
                content: "checkpoint question".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "checkpoint answer".to_string(),
            },
        ];
        let branch_a_history = checkpoint
            .iter()
            .cloned()
            .chain(std::iter::once(ChatMessage {
                role: Role::User,
                content: "branch A only".to_string(),
            }))
            .collect::<Vec<_>>();
        let branch_b_history = checkpoint
            .iter()
            .cloned()
            .chain(std::iter::once(ChatMessage {
                role: Role::User,
                content: "branch B only".to_string(),
            }))
            .collect::<Vec<_>>();
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "branch B answer".to_string(),
                "branch A answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let config = MemoryConfig {
            strategy: MemoryStrategy::Branching,
            recent_messages: 8,
            ..MemoryConfig::default()
        };
        let mut branch_a = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            branch_a_history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        branch_a.set_memory_config(config.clone());
        let mut branch_b = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            branch_b_history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        branch_b.set_memory_config(config);

        branch_a
            .respond(&client, "continue A".to_string())
            .await
            .expect("branch A response");
        branch_b
            .respond(&client, "continue B".to_string())
            .await
            .expect("branch B response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0]
                .iter()
                .any(|message| message.content == "branch A only")
        );
        assert!(
            seen[0]
                .iter()
                .all(|message| message.content != "branch B only")
        );
        assert!(
            seen[1]
                .iter()
                .any(|message| message.content == "branch B only")
        );
        assert!(
            seen[1]
                .iter()
                .all(|message| message.content != "branch A only")
        );
    }

    #[tokio::test]
    async fn scoped_branches_keep_one_session_but_filter_provider_context() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "beta answer".to_string(),
                "alpha answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            Vec::new(),
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::ScopedBranches,
            recent_messages: 8,
            active_branch: "alpha".to_string(),
            scoped_auto_route: false,
            ..MemoryConfig::default()
        });
        agent
            .respond(&client, "alpha question".to_string())
            .await
            .expect("alpha response");

        let mut beta_config = agent.memory_config();
        beta_config.active_branch = "beta".to_string();
        agent.set_memory_config(beta_config);
        agent
            .respond(&client, "beta question".to_string())
            .await
            .expect("beta response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0]
                .iter()
                .any(|message| message.content == "alpha question")
        );
        assert!(
            seen[1]
                .iter()
                .any(|message| message.content == "beta question")
        );
        assert!(
            seen[1]
                .iter()
                .all(|message| !message.content.contains("alpha"))
        );
        assert!(
            agent
                .history()
                .iter()
                .any(|message| message.content == "alpha question")
        );
        assert!(
            agent
                .history()
                .iter()
                .any(|message| message.content == "beta question")
        );
        assert_eq!(
            agent
                .memory()
                .branch_assignments
                .get(&0)
                .map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            agent
                .memory()
                .branch_assignments
                .get(&2)
                .map(String::as_str),
            Some("beta")
        );
    }

    #[tokio::test]
    async fn scoped_branches_auto_route_back_to_existing_topic() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = vec![
            ChatMessage {
                role: Role::User,
                content: "Rust async borrow checker problem".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "Use ownership boundaries.".to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: "Vacation budget and hotel plan".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "Track flights and hotels.".to_string(),
            },
        ];
        let mut memory = AgentMemory::default();
        memory
            .branch_assignments
            .insert(0, "rust async".to_string());
        memory
            .branch_assignments
            .insert(1, "rust async".to_string());
        memory
            .branch_assignments
            .insert(2, "vacation budget".to_string());
        memory
            .branch_assignments
            .insert(3, "vacation budget".to_string());
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            memory,
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::ScopedBranches,
            recent_messages: 8,
            active_branch: "vacation budget".to_string(),
            scoped_auto_route: true,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "Back to Rust async ownership".to_string())
            .await
            .expect("response");

        assert_eq!(agent.memory_config().active_branch, "rust async");
        let seen = client.seen_messages.lock().expect("seen messages");
        assert!(
            seen[0]
                .iter()
                .any(|message| message.content.contains("Rust async"))
        );
        assert!(
            seen[0]
                .iter()
                .all(|message| !message.content.contains("Vacation budget"))
        );
    }

    #[tokio::test]
    async fn scoped_branches_allow_manual_topic_when_auto_route_is_off() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let history = vec![ChatMessage {
            role: Role::User,
            content: "Rust async borrow checker problem".to_string(),
        }];
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::ScopedBranches,
            recent_messages: 8,
            active_branch: "manual finance".to_string(),
            scoped_auto_route: false,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "Rust async ownership followup".to_string())
            .await
            .expect("response");

        assert_eq!(agent.memory_config().active_branch, "manual finance");
        assert_eq!(
            agent
                .memory()
                .branch_assignments
                .get(&1)
                .map(String::as_str),
            Some("manual finance")
        );
    }

    #[tokio::test]
    async fn agent_uses_custom_system_prompt_without_default_agent_prompt() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(Vec::new()),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            vec![ChatMessage {
                role: Role::System,
                content: "Ты эксперт по Civilization 6.".to_string(),
            }],
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );

        agent
            .respond(&client, "Как играть за Византию?".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen[0].len(), 2);
        assert_eq!(seen[0][0].role, Role::System);
        assert_eq!(seen[0][0].content, "Ты эксперт по Civilization 6.");
        assert_eq!(seen[0][1].content, "Как играть за Византию?");
    }

    #[test]
    fn agent_registry_selects_known_agent() {
        let agent = selected_agent(Some(LOCAL_SESSION_AGENT_ID)).expect("agent");

        assert_eq!(agent.id, LOCAL_SESSION_AGENT_ID);
        assert!(!available_agents().is_empty());
        assert!(matches!(
            selected_agent(Some("missing-agent")),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn agent_gracefully_degrades_when_context_overflows() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["answer".to_string()]),
            metrics: std::sync::Mutex::new(vec![crate::providers::RequestMetrics {
                elapsed_ms: 1,
                usage: None,
                cost: None,
            }]),
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };

        let history = (0..50)
            .map(|index| ChatMessage {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: "x".repeat(500),
            })
            .collect::<Vec<_>>();

        let original_history_len = history.len();

        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            history,
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_context_limit(Some(512));
        agent.set_memory_config(MemoryConfig {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: 12,
            ..MemoryConfig::default()
        });

        let response = agent
            .respond(&client, "new question".to_string())
            .await
            .expect("response");

        assert!(!response.text.is_empty(), "response should be generated");

        let seen = client.seen_messages.lock().expect("seen");
        assert!(!seen.is_empty(), "should have sent at least one request");

        let main_request = &seen[seen.len() - 1];
        assert!(
            main_request.len() < original_history_len + 1,
            "context window strategy should limit messages sent (not send all 50+ messages). sent {} messages out of {}",
            main_request.len(),
            original_history_len
        );
    }

    #[test]
    fn agent_context_limit_can_be_updated() {
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            Vec::new(),
            AgentMemory::default(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );

        assert!(agent.context_limit().is_none());
        agent.set_context_limit(Some(8192));
        assert_eq!(agent.context_limit(), Some(8192));
    }
}
