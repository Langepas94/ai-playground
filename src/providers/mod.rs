use std::{env, fs, path::PathBuf};

use async_trait::async_trait;
use reqwest::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
};

pub mod deepseek;
pub mod gigachat;
pub mod kimi;
pub mod openai_compatible;
pub mod openrouter;

const DEFAULT_CA_BUNDLE_FILE: &str = "russian_trusted_root_ca_pem.crt";

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
    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::OpenAiCompatible,
            Self::OpenRouter,
            Self::DeepSeek,
            Self::GigaChat,
            Self::Kimi,
        ]
    }

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
    pub suggested_models: &'static [&'static str],
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnswerFormat {
    #[default]
    Natural,
    Bullets,
    Numbered,
    Short,
    Steps,
    Table,
}

impl std::fmt::Display for AnswerFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Natural => write!(f, "natural"),
            Self::Bullets => write!(f, "bullets"),
            Self::Numbered => write!(f, "numbered"),
            Self::Short => write!(f, "short"),
            Self::Steps => write!(f, "steps"),
            Self::Table => write!(f, "table"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseControl {
    pub format: ResponseFormat,
    pub answer_format: AnswerFormat,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub answer_prefix: Option<String>,
    pub answer_suffix: Option<String>,
    pub address_as: Option<String>,
    pub quote_question: bool,
    pub format_instruction: Option<String>,
    pub completion_instruction: Option<String>,
}

impl ResponseControl {
    pub fn uncontrolled() -> Self {
        Self::default()
    }

    pub fn is_uncontrolled(&self) -> bool {
        self.format == ResponseFormat::Text
            && self.answer_format == AnswerFormat::Natural
            && self.max_tokens.is_none()
            && self.stop.is_empty()
            && self.answer_prefix.is_none()
            && self.answer_suffix.is_none()
            && self.address_as.is_none()
            && !self.quote_question
            && self.format_instruction.is_none()
            && self.completion_instruction.is_none()
    }

    pub fn instruction_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        if let Some(instruction) = self.answer_format_instruction() {
            messages.push(ChatMessage {
                role: Role::System,
                content: instruction,
            });
        }
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

    fn answer_format_instruction(&self) -> Option<String> {
        let mut rules = Vec::new();
        match self.answer_format {
            AnswerFormat::Natural => {}
            AnswerFormat::Bullets => rules.push("Answer as concise bullet points.".to_string()),
            AnswerFormat::Numbered => rules.push("Answer as a numbered list.".to_string()),
            AnswerFormat::Short => rules.push("Answer in one or two short sentences.".to_string()),
            AnswerFormat::Steps => {
                rules.push("Answer as clear step-by-step instructions.".to_string())
            }
            AnswerFormat::Table => {
                rules.push("Answer as a Markdown table when possible.".to_string())
            }
        }
        if self.quote_question {
            rules.push("Start by quoting the user's question in one short line.".to_string());
        }
        if let Some(name) = &self.address_as {
            rules.push(format!(
                "Address the user as \"{name}\" at the start of the answer."
            ));
        }
        if let Some(prefix) = &self.answer_prefix {
            rules.push(format!("Start the answer exactly with this text: {prefix}"));
        }
        if let Some(suffix) = &self.answer_suffix {
            rules.push(format!("End the answer exactly with this text: {suffix}"));
        }
        if rules.is_empty() {
            None
        } else {
            Some(format!(
                "Answer formatting rules:\n- {}",
                rules.join("\n- ")
            ))
        }
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
        let builder = Client::builder().timeout(std::time::Duration::from_secs(60));
        let client = add_extra_root_certificates(builder)?
            .build()
            .map_err(AppError::from)?;
        Ok(Self { client })
    }
}

fn add_extra_root_certificates(mut builder: ClientBuilder) -> Result<ClientBuilder, AppError> {
    let Some(path) = extra_ca_bundle_path()? else {
        return Ok(builder);
    };

    let pem = fs::read(&path).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("could not read CA bundle: {error}"),
    })?;
    let certificates =
        reqwest::Certificate::from_pem_bundle(&pem).map_err(|error| AppError::Config {
            path: path.clone(),
            message: format!("could not parse PEM certificates from CA bundle: {error}"),
        })?;

    for certificate in certificates {
        builder = builder.add_root_certificate(certificate);
    }
    Ok(builder)
}

fn extra_ca_bundle_path() -> Result<Option<PathBuf>, AppError> {
    if let Some(path) = env::var_os("AITEACH_CA_BUNDLE") {
        return Ok(Some(PathBuf::from(path)));
    }

    let Some(config_dir) = AppConfig::config_path()?.parent().map(ToOwned::to_owned) else {
        return Ok(None);
    };
    let path = config_dir.join(DEFAULT_CA_BUNDLE_FILE);
    Ok(path.exists().then_some(path))
}

#[async_trait]
impl ProviderClient for ReqwestProviderClient {
    async fn list_models(
        &self,
        profile: &ProfileConfig,
        token: &str,
    ) -> Result<Vec<String>, AppError> {
        let spec = profile.provider.spec();
        let token = self.bearer_token(profile, token).await?;
        openai_compatible::list_models(&self.client, spec, profile, &token).await
    }

    async fn chat_completion(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError> {
        let spec = profile.provider.spec();
        let token = self.bearer_token(profile, token).await?;
        openai_compatible::chat_completion(&self.client, spec, profile, &token, request).await
    }
}

impl ReqwestProviderClient {
    async fn bearer_token(&self, profile: &ProfileConfig, token: &str) -> Result<String, AppError> {
        match profile.provider {
            ProviderKind::GigaChat => gigachat::bearer_token(&self.client, token).await,
            _ => Ok(token.to_string()),
        }
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
                answer_format: AnswerFormat::Bullets,
                max_tokens: Some(64),
                stop: vec!["END".to_string()],
                answer_prefix: Some("Artem,".to_string()),
                answer_suffix: None,
                address_as: None,
                quote_question: true,
                format_instruction: None,
                completion_instruction: Some("Stop after the summary field.".to_string()),
            },
        });

        assert_eq!(payload.max_tokens, Some(64));
        assert_eq!(payload.stop, vec!["END"]);
        assert_eq!(payload.response_format.expect("format").kind, "json_object");
        assert_eq!(payload.messages[0].role, Role::System);
        assert!(payload.messages[0].content.contains("bullet points"));
        assert!(payload.messages[0].content.contains("Artem"));
        assert!(payload.messages[1].content.contains("valid JSON object"));
        assert_eq!(payload.messages[3].role, Role::User);
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
