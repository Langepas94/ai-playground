use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, ResponseControl, Role,
    },
};

#[derive(Debug, Clone)]
pub struct ChatAgent {
    profile: ProfileConfig,
    token: String,
    history: Vec<ChatMessage>,
    control: ResponseControl,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
}

impl ChatAgent {
    pub fn new(
        profile: ProfileConfig,
        token: String,
        history: Vec<ChatMessage>,
        control: ResponseControl,
        pricing: Option<ModelPricing>,
        billing: Option<BillingLookup>,
    ) -> Self {
        Self {
            profile,
            token,
            history,
            control,
            pricing,
            billing,
        }
    }

    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn set_control(&mut self, control: ResponseControl) {
        self.control = control;
    }

    pub async fn respond(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<ChatResponse, AppError> {
        let response = client
            .chat_completion(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        Ok(response)
    }

    pub async fn respond_with_debug(
        &mut self,
        client: &dyn ProviderClient,
        prompt: String,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let (response, debug) = client
            .chat_completion_with_debug(
                &self.profile,
                &self.token,
                self.request_with_user_prompt(prompt.clone()),
            )
            .await?;
        self.commit_turn(prompt, response.text.clone());
        Ok((response, debug))
    }

    pub fn control(&self) -> &ResponseControl {
        &self.control
    }

    fn request_with_user_prompt(&self, prompt: String) -> ChatRequest {
        let mut messages = self.history.clone();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Debug, Default)]
    struct FakeClient {
        replies: std::sync::Mutex<Vec<String>>,
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
            Ok(ChatResponse {
                text,
                finish_reason: Some("stop".to_string()),
                metrics: crate::providers::RequestMetrics {
                    elapsed_ms: 1,
                    usage: None,
                    cost: None,
                },
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
            seen_messages: std::sync::Mutex::new(Vec::new()),
        };
        let mut agent = ChatAgent::new(
            test_profile(),
            "secret".to_string(),
            Vec::new(),
            ResponseControl::uncontrolled(),
            None,
            None,
        );

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
        assert_eq!(seen[1].len(), 3);
        assert_eq!(seen[1][0].content, "first question");
        assert_eq!(seen[1][1].content, "first answer");
        assert_eq!(seen[1][2].content, "second question");
        assert_eq!(agent.history().len(), 4);
    }
}
