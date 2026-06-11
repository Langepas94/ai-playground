use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, ResponseControl, Role,
    },
};

use super::memory::{AgentMemory, MemoryConfig, format_messages_for_summary};
use super::token_accounting::{TokenEstimate, estimate_exchange, estimate_messages_tokens};
use tokio::time::{Duration, timeout};

pub const LOCAL_SESSION_AGENT_ID: &str = "local-session-agent";
const MEMORY_REFRESH_TIMEOUT: Duration = Duration::from_millis(250);
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
        self.memory_config
    }

    pub fn set_memory_config(&mut self, memory_config: MemoryConfig) {
        self.memory_config = memory_config;
    }

    pub fn set_context_limit(&mut self, context_limit: Option<u32>) {
        self.context_limit = context_limit.filter(|limit| *limit > 0);
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
        let summary_metrics = self.preflight_compact_with_timeout(client, &prompt).await;
        let response = client
            .chat_completion(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        let mut response = response;
        if let Some(summary_metrics) = summary_metrics {
            response.metrics =
                crate::chat::add_request_metrics(&response.metrics, &summary_metrics);
        }
        if let Some(summary_metrics) = self.refresh_memory_with_timeout(client).await {
            response.metrics =
                crate::chat::add_request_metrics(&response.metrics, &summary_metrics);
        }
        Ok(response)
    }

    pub async fn respond_with_debug(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let summary_metrics = self.preflight_compact_with_timeout(client, &prompt).await;
        let (response, debug) = client
            .chat_completion_with_debug(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        let mut response = response;
        if let Some(summary_metrics) = summary_metrics {
            response.metrics =
                crate::chat::add_request_metrics(&response.metrics, &summary_metrics);
        }
        if let Some(summary_metrics) = self.refresh_memory_with_timeout(client).await {
            response.metrics =
                crate::chat::add_request_metrics(&response.metrics, &summary_metrics);
        }
        Ok((response, debug))
    }

    pub async fn prepare_stream_request(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> (
        crate::providers::ChatRequest,
        Option<crate::providers::RequestMetrics>,
    ) {
        let summary_metrics = self.preflight_compact_with_timeout(client, prompt).await;
        (
            self.request_with_user_prompt(prompt.to_string()),
            summary_metrics,
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
        let mut messages = self.memory.build_context(&self.history, self.memory_config);
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
        self.history.push(ChatMessage {
            role: Role::User,
            content: prompt,
        });
        self.history.push(ChatMessage {
            role: Role::Assistant,
            content: answer,
        });
    }

    async fn refresh_memory_with_timeout(
        &mut self,
        client: &dyn ProviderClient,
    ) -> Option<crate::providers::RequestMetrics> {
        timeout(MEMORY_REFRESH_TIMEOUT, self.refresh_memory(client))
            .await
            .ok()
            .flatten()
    }

    async fn refresh_memory(
        &mut self,
        client: &dyn ProviderClient,
    ) -> Option<crate::providers::RequestMetrics> {
        let Some(range) = self
            .memory
            .next_summary_range(&self.history, self.memory_config)
        else {
            return None;
        };
        self.compact_history_range(client, range).await
    }

    async fn preflight_compact_with_timeout(
        &mut self,
        client: &dyn ProviderClient,
        prompt: &str,
    ) -> Option<crate::providers::RequestMetrics> {
        timeout(
            MEMORY_REFRESH_TIMEOUT,
            self.preflight_compact_for_context_pressure(client, prompt),
        )
        .await
        .ok()
        .flatten()
    }

    async fn preflight_compact_for_context_pressure(
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
            let Some(range) = self.memory.next_summary_range_for_pressure(
                &self.history,
                self.memory_config,
                self.memory_config.recent_messages,
            ) else {
                break;
            };
            let summary_metrics = self.compact_history_range(client, range).await?;
            accumulated = Some(match accumulated {
                Some(current) => crate::chat::add_request_metrics(&current, &summary_metrics),
                None => summary_metrics,
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
        let summary_request = ChatRequest {
            model: self.profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: "You are the memory compaction module of a local chat agent. Update the session memory summary using only the supplied facts. Keep durable user preferences, goals, decisions, constraints, and unresolved context. Be concise. Do not invent facts.".to_string(),
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
    async fn agent_accumulates_chat_history_between_turns() {
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
            recent_messages: 12,
            summarize_after_messages: 18,
            summary_chunk_messages: 1,
            ..MemoryConfig::default()
        });

        agent
            .respond(&client, "first question".to_string())
            .await
            .expect("first response");
        agent
            .respond(&client, "second question".to_string())
            .await
            .expect("second response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen[0].len(), 1);
        assert_eq!(seen[0][0].content, "first question");
        assert_eq!(seen[1].len(), 3);
        assert_eq!(seen[1][0].content, "first question");
        assert_eq!(seen[1][1].content, "first answer");
        assert_eq!(seen[1][2].content, "second question");
        assert_eq!(agent.history().len(), 4);
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
    async fn agent_sends_layered_context_instead_of_full_long_history() {
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

        agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].len(), 13);
        assert_eq!(seen[0][0].content, "history 8");
        assert_eq!(seen[0][12].content, "current question");
        assert_eq!(seen[1].len(), 2);
        assert!(seen[1][1].content.contains("history 0"));
        assert_eq!(
            agent.memory().session_summary.as_deref(),
            Some("summary of older turns")
        );
    }

    #[tokio::test]
    async fn agent_preflight_summarizes_when_context_pressure_is_high() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "final answer".to_string(),
                "pressure summary".to_string(),
            ]),
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
                content: format!("history message {index}"),
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
        agent.set_context_limit(Some(120));
        agent.set_memory_config(MemoryConfig {
            recent_messages: 4,
            summarize_after_messages: 99,
            summary_chunk_messages: 10,
            summarize_at_context_percent: 80,
            ..MemoryConfig::default()
        });

        let prompt = (0..55)
            .map(|index| format!("huge{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        agent.respond(&client, prompt).await.expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0][0].role, Role::System);
        assert!(seen[0][0].content.contains("memory compaction module"));
        assert!(
            seen[1]
                .iter()
                .any(|message| message.content.contains("pressure summary"))
        );
        assert!(
            seen[1]
                .iter()
                .all(|message| !message.content.contains("history message 0"))
        );
        assert_eq!(
            agent.memory().session_summary.as_deref(),
            Some("pressure summary")
        );
        assert_eq!(agent.memory().summarized_message_count, 2);
    }

    #[tokio::test]
    async fn agent_full_memory_strategy_sends_complete_history() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec!["fresh answer".to_string()]),
            metrics: std::sync::Mutex::new(vec![crate::providers::RequestMetrics {
                elapsed_ms: 1,
                usage: None,
                cost: Some(crate::providers::RequestCost {
                    amount: 0.1,
                    currency: "USD".to_string(),
                    source: crate::providers::CostSource::ConfiguredPricing,
                }),
            }]),
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
            AgentMemory {
                session_summary: Some("summary should not be sent".to_string()),
                summarized_message_count: 8,
            },
            ResponseControl::uncontrolled(),
            None,
            None,
        );
        agent.set_memory_config(MemoryConfig {
            strategy: crate::chat::memory::MemoryStrategy::Full,
            recent_messages: 2,
            summarize_after_messages: 4,
            summary_chunk_messages: 1,
            summarize_at_context_percent: 80,
        });

        agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let seen = client.seen_messages.lock().expect("seen messages");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].len(), 21);
        assert_eq!(seen[0][0].content, "history 0");
        assert_eq!(seen[0][20].content, "current question");
        assert_eq!(
            agent.memory().session_summary.as_deref(),
            Some("summary should not be sent")
        );
    }

    #[tokio::test]
    async fn agent_adds_summary_cost_into_final_response_metrics() {
        let client = FakeClient {
            replies: std::sync::Mutex::new(vec![
                "summary of older turns".to_string(),
                "final answer".to_string(),
            ]),
            metrics: std::sync::Mutex::new(vec![
                crate::providers::RequestMetrics {
                    elapsed_ms: 1,
                    usage: None,
                    cost: Some(crate::providers::RequestCost {
                        amount: 0.25,
                        currency: "USD".to_string(),
                        source: crate::providers::CostSource::ConfiguredPricing,
                    }),
                },
                crate::providers::RequestMetrics {
                    elapsed_ms: 2,
                    usage: None,
                    cost: Some(crate::providers::RequestCost {
                        amount: 0.75,
                        currency: "USD".to_string(),
                        source: crate::providers::CostSource::ConfiguredPricing,
                    }),
                },
            ]),
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

        let response = agent
            .respond(&client, "current question".to_string())
            .await
            .expect("response");

        let cost = response.metrics.cost.expect("combined cost");
        assert!((cost.amount - 1.0).abs() < f64::EPSILON);
        assert_eq!(cost.currency, "USD");
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
}
