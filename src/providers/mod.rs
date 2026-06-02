use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{config::ProfileConfig, errors::AppError};

pub mod deepseek;
pub mod gigachat;
pub mod kimi;
pub mod openai_compatible;
pub mod openrouter;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenAiCompatible,
    OpenRouter,
    DeepSeek,
    GigaChat,
    Kimi,
}

impl std::str::FromStr for ProviderKind {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "openai-compatible" | "openai" | "compatible" => Ok(Self::OpenAiCompatible),
            "openrouter" => Ok(Self::OpenRouter),
            "deepseek" => Ok(Self::DeepSeek),
            "gigachat" => Ok(Self::GigaChat),
            "kimi" | "moonshot" => Ok(Self::Kimi),
            other => Err(AppError::UnsupportedProvider(other.to_string())),
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::OpenRouter => "openrouter",
            Self::DeepSeek => "deepseek",
            Self::GigaChat => "gigachat",
            Self::Kimi => "kimi",
        };
        write!(f, "{value}")
    }
}

impl ProviderKind {
    pub fn spec(&self) -> ProviderSpec {
        match self {
            Self::OpenAiCompatible => openai_compatible::spec(),
            Self::OpenRouter => openrouter::spec(),
            Self::DeepSeek => deepseek::spec(),
            Self::GigaChat => gigachat::spec(),
            Self::Kimi => kimi::spec(),
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        self.spec().default_base_url
    }

    pub fn default_model(&self) -> &'static str {
        self.spec().default_model
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    Bearer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticHeader {
    pub name: &'static str,
    pub value: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    pub kind: ProviderKind,
    pub display_name: &'static str,
    pub default_base_url: &'static str,
    pub default_model: &'static str,
    pub auth_scheme: AuthScheme,
    pub extra_headers: &'static [StaticHeader],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
}

impl std::fmt::Display for ResponseFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::JsonObject => write!(f, "json-object"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseControl {
    pub format: ResponseFormat,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub format_instruction: Option<String>,
    pub completion_instruction: Option<String>,
}

impl ResponseControl {
    pub fn uncontrolled() -> Self {
        Self::default()
    }

    pub fn is_uncontrolled(&self) -> bool {
        self.format == ResponseFormat::Text
            && self.max_tokens.is_none()
            && self.stop.is_empty()
            && self.format_instruction.is_none()
            && self.completion_instruction.is_none()
    }

    pub fn instruction_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        match self.format {
            ResponseFormat::Text => {
                if let Some(instruction) = &self.format_instruction {
                    messages.push(ChatMessage {
                        role: Role::System,
                        content: instruction.clone(),
                    });
                }
            }
            ResponseFormat::JsonObject => {
                messages.push(ChatMessage {
                    role: Role::System,
                    content: self.format_instruction.clone().unwrap_or_else(|| {
                        "Return only a valid JSON object. Do not include Markdown, prose, or code fences."
                            .to_string()
                    }),
                });
            }
        }
        if let Some(instruction) = &self.completion_instruction {
            messages.push(ChatMessage {
                role: Role::System,
                content: instruction.clone(),
            });
        }
        messages
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub control: ResponseControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
    pub finish_reason: Option<String>,
}

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn list_models(
        &self,
        profile: &ProfileConfig,
        token: &str,
    ) -> Result<Vec<String>, AppError>;
    async fn chat_completion(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestProviderClient {
    client: Client,
}

impl ReqwestProviderClient {
    pub fn new() -> Result<Self, AppError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(AppError::from)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl ProviderClient for ReqwestProviderClient {
    async fn list_models(
        &self,
        profile: &ProfileConfig,
        token: &str,
    ) -> Result<Vec<String>, AppError> {
        let spec = profile.provider.spec();
        openai_compatible::list_models(&self.client, spec, profile, token).await
    }

    async fn chat_completion(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError> {
        let spec = profile.provider.spec();
        openai_compatible::chat_completion(&self.client, spec, profile, token, request).await
    }
}

pub fn validate_base_url(profile_name: &str, base_url: &str) -> Result<(), AppError> {
    let parsed = reqwest::Url::parse(base_url).map_err(|_| AppError::InvalidBaseUrl {
        profile: profile_name.to_string(),
        url: base_url.to_string(),
    })?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(AppError::InvalidBaseUrl {
            profile: profile_name.to_string(),
            url: base_url.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{EndpointCategory, HttpProblem};
    use reqwest::StatusCode;

    #[test]
    fn provider_request_mapping_matches_openai_shape() {
        let payload = openai_compatible::chat_payload(ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "hello".to_string(),
            }],
            control: ResponseControl::uncontrolled(),
        });

        assert_eq!(payload.model, "m");
        assert_eq!(payload.messages[0].role, Role::User);
        assert_eq!(payload.messages[0].content, "hello");
    }

    #[test]
    fn provider_request_mapping_includes_response_control() {
        let payload = openai_compatible::chat_payload(ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "return data".to_string(),
            }],
            control: ResponseControl {
                format: ResponseFormat::JsonObject,
                max_tokens: Some(64),
                stop: vec!["END".to_string()],
                format_instruction: None,
                completion_instruction: Some("Stop after the summary field.".to_string()),
            },
        });

        assert_eq!(payload.max_tokens, Some(64));
        assert_eq!(payload.stop, vec!["END"]);
        assert_eq!(payload.response_format.expect("format").kind, "json_object");
        assert_eq!(payload.messages[0].role, Role::System);
        assert!(payload.messages[0].content.contains("valid JSON object"));
        assert_eq!(payload.messages[2].role, Role::User);
    }

    #[test]
    fn provider_registry_keeps_provider_specific_defaults() {
        let openrouter = ProviderKind::OpenRouter.spec();
        let deepseek = ProviderKind::DeepSeek.spec();

        assert_eq!(openrouter.kind, ProviderKind::OpenRouter);
        assert_eq!(openrouter.default_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(deepseek.default_model, "deepseek-chat");
    }

    #[test]
    fn http_error_mapping_identifies_auth_and_rate_limit() {
        let auth = crate::errors::map_http_status(
            "deepseek",
            EndpointCategory::Chat,
            StatusCode::UNAUTHORIZED,
            None,
            "bad token",
        );
        assert_eq!(auth.problem, HttpProblem::Auth);

        let limited = crate::errors::map_http_status(
            "kimi",
            EndpointCategory::Models,
            StatusCode::TOO_MANY_REQUESTS,
            Some("3".to_string()),
            "slow down",
        );
        assert_eq!(
            limited.problem,
            HttpProblem::RateLimit {
                retry_after: Some("3".to_string())
            }
        );
    }
}
