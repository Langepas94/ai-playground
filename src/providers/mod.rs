use async_trait::async_trait;
use reqwest::{Client, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};

use crate::{
    config::ProfileConfig,
    errors::{AppError, EndpointCategory, HttpProblem, ProviderHttpError, map_http_status},
};

pub mod deepseek;
pub mod gigachat;
pub mod kimi;
pub mod openai_compatible;
pub mod openrouter;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
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
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "https://api.openai.com/v1",
            Self::OpenRouter => openrouter::BASE_URL,
            Self::DeepSeek => deepseek::BASE_URL,
            Self::GigaChat => gigachat::BASE_URL,
            Self::Kimi => kimi::BASE_URL,
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "gpt-4.1-mini",
            Self::OpenRouter => "openai/gpt-4.1-mini",
            Self::DeepSeek => "deepseek-chat",
            Self::GigaChat => "GigaChat",
            Self::Kimi => "moonshot-v1-8k",
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
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
        openai_compatible::list_models(&self.client, profile, token).await
    }

    async fn chat_completion(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError> {
        openai_compatible::chat_completion(&self.client, profile, token, request).await
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

async fn response_text_or_error(
    response: reqwest::Response,
    provider: &ProviderKind,
    endpoint: EndpointCategory,
) -> Result<String, AppError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let body = response.text().await.map_err(AppError::from)?;
    if status.is_success() {
        return Ok(body);
    }
    Err(AppError::ProviderHttp(map_http_status(
        provider.to_string(),
        endpoint,
        status,
        retry_after,
        short_reason(&body),
    )))
}

pub fn map_network_error(
    provider: &ProviderKind,
    endpoint: EndpointCategory,
    error: reqwest::Error,
) -> AppError {
    AppError::ProviderHttp(ProviderHttpError {
        provider: provider.to_string(),
        endpoint,
        status: error.status(),
        problem: if error.is_decode() {
            HttpProblem::UnexpectedFormat
        } else {
            HttpProblem::Network
        },
        reason: error.to_string(),
    })
}

fn short_reason(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 180 {
        format!("{}...", collapsed.chars().take(180).collect::<String>())
    } else if collapsed.is_empty() {
        "empty response body".to_string()
    } else {
        collapsed
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiChatPayload {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoiceMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

pub fn chat_payload(request: ChatRequest) -> OpenAiChatPayload {
    OpenAiChatPayload {
        model: request.model,
        messages: request.messages,
    }
}

fn parse_chat_response(provider: &ProviderKind, raw: &str) -> Result<ChatResponse, AppError> {
    let parsed: OpenAiChatResponse = serde_json::from_str(raw).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: provider.to_string(),
            endpoint: EndpointCategory::Chat,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    let text = parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            AppError::ProviderHttp(ProviderHttpError {
                provider: provider.to_string(),
                endpoint: EndpointCategory::Chat,
                status: Some(StatusCode::OK),
                problem: HttpProblem::UnexpectedFormat,
                reason: "missing choices[0].message.content".to_string(),
            })
        })?;
    Ok(ChatResponse { text })
}

fn parse_models_response(provider: &ProviderKind, raw: &str) -> Result<Vec<String>, AppError> {
    let parsed: ModelsResponse = serde_json::from_str(raw).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: provider.to_string(),
            endpoint: EndpointCategory::Models,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    Ok(parsed.data.into_iter().map(|model| model.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_request_mapping_matches_openai_shape() {
        let payload = chat_payload(ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "hello".to_string(),
            }],
        });

        assert_eq!(payload.model, "m");
        assert_eq!(payload.messages[0].role, Role::User);
        assert_eq!(payload.messages[0].content, "hello");
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
