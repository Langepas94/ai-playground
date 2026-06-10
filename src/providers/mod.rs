use std::{collections::BTreeMap, env, fs, path::PathBuf};

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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
pub enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
    Toon,
}

impl std::fmt::Display for ResponseFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::JsonObject => write!(f, "json-object"),
            Self::Toon => write!(f, "toon"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
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

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ResponseControl {
    pub format: ResponseFormat,
    pub answer_format: AnswerFormat,
    pub max_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    pub top_a: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub repetition_penalty: Option<f32>,
    pub seed: Option<i64>,
    pub reasoning_effort: Option<String>,
    pub include_reasoning: Option<bool>,
    pub verbosity: Option<String>,
    pub logprobs: Option<bool>,
    pub top_logprobs: Option<u32>,
    pub n: Option<u32>,
    pub store: Option<bool>,
    pub parallel_tool_calls: Option<bool>,
    pub user: Option<String>,
    pub service_tier: Option<String>,
    pub extra_params: serde_json::Map<String, serde_json::Value>,
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
            && self.max_completion_tokens.is_none()
            && self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.min_p.is_none()
            && self.top_a.is_none()
            && self.presence_penalty.is_none()
            && self.frequency_penalty.is_none()
            && self.repetition_penalty.is_none()
            && self.seed.is_none()
            && self.reasoning_effort.is_none()
            && self.include_reasoning.is_none()
            && self.verbosity.is_none()
            && self.logprobs.is_none()
            && self.top_logprobs.is_none()
            && self.n.is_none()
            && self.store.is_none()
            && self.parallel_tool_calls.is_none()
            && self.user.is_none()
            && self.service_tier.is_none()
            && self.extra_params.is_empty()
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
            ResponseFormat::Toon => {
                messages.push(ChatMessage {
                    role: Role::System,
                    content: self.format_instruction.clone().unwrap_or_else(|| {
                        "Return only a valid TOON document. Do not include Markdown, prose, JSON, or code fences."
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub control: ResponseControl,
    pub pricing: Option<ModelPricing>,
    pub billing: Option<BillingLookup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_input_tokens: Option<u32>,
    pub cache_miss_input_tokens: Option<u32>,
    pub input_audio_tokens: Option<u32>,
    pub output_reasoning_tokens: Option<u32>,
    pub output_visible_tokens: Option<u32>,
    pub output_audio_tokens: Option<u32>,
    pub accepted_prediction_output_tokens: Option<u32>,
    pub rejected_prediction_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelPricing {
    pub currency: String,
    pub input_per_million: Option<f64>,
    pub output_per_million: f64,
    pub cache_hit_input_per_million: Option<f64>,
    pub cache_miss_input_per_million: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub pricing: Option<ModelPricing>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BillingLookup {
    pub provider: BillingProvider,
    pub admin_token: String,
    pub poll_seconds: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BillingProvider {
    OpenAiCosts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestCost {
    pub amount: f64,
    pub currency: String,
    pub source: CostSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CostSource {
    ProviderReported,
    ConfiguredPricing,
    BillingApi,
}

impl std::fmt::Display for CostSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderReported => write!(f, "provider-reported"),
            Self::ConfiguredPricing => write!(f, "configured-pricing"),
            Self::BillingApi => write!(f, "billing-api"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RequestMetrics {
    pub elapsed_ms: u128,
    pub usage: Option<TokenUsage>,
    pub cost: Option<RequestCost>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub text: String,
    pub finish_reason: Option<String>,
    pub metrics: RequestMetrics,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderExchangeDebug {
    pub request: HttpDebugRequest,
    pub response: HttpDebugResponse,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HttpDebugRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HttpDebugResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
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
    async fn chat_completion_with_debug(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestProviderClient {
    client: Client,
    gigachat_tokens: gigachat::GigaChatTokenCache,
    gigachat_oauth_url: String,
}

impl ReqwestProviderClient {
    pub fn new() -> Result<Self, AppError> {
        Self::new_with_gigachat_oauth_url(gigachat::default_oauth_url())
    }

    #[doc(hidden)]
    pub fn new_with_gigachat_oauth_url(gigachat_oauth_url: String) -> Result<Self, AppError> {
        let builder = Client::builder().timeout(std::time::Duration::from_secs(300));
        let client = add_extra_root_certificates(builder)?
            .build()
            .map_err(AppError::from)?;
        Ok(Self {
            client,
            gigachat_tokens: gigachat::GigaChatTokenCache::default(),
            gigachat_oauth_url,
        })
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
    if let Some(path) =
        env::var_os("AI_PLAYGROUND_CA_BUNDLE").or_else(|| env::var_os("AITEACH_CA_BUNDLE"))
    {
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
        let access_token = self.bearer_token(profile, token).await?;
        let result =
            openai_compatible::list_models(&self.client, spec, profile, &access_token).await;
        if self.should_refresh_gigachat_token(profile, token, &result) {
            let access_token = self.refresh_bearer_token(profile, token).await?;
            return openai_compatible::list_models(&self.client, spec, profile, &access_token)
                .await;
        }
        result
    }

    async fn chat_completion(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError> {
        let spec = profile.provider.spec();
        let access_token = self.bearer_token(profile, token).await?;
        let result = openai_compatible::chat_completion(
            &self.client,
            spec,
            profile,
            &access_token,
            request.clone(),
        )
        .await;
        if self.should_refresh_gigachat_token(profile, token, &result) {
            let access_token = self.refresh_bearer_token(profile, token).await?;
            return openai_compatible::chat_completion(
                &self.client,
                spec,
                profile,
                &access_token,
                request,
            )
            .await;
        }
        result
    }

    async fn chat_completion_with_debug(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let spec = profile.provider.spec();
        let access_token = self.bearer_token(profile, token).await?;
        let result = openai_compatible::chat_completion_with_debug(
            &self.client,
            spec,
            profile,
            &access_token,
            request.clone(),
        )
        .await;
        if self.should_refresh_gigachat_token(profile, token, &result) {
            let access_token = self.refresh_bearer_token(profile, token).await?;
            return openai_compatible::chat_completion_with_debug(
                &self.client,
                spec,
                profile,
                &access_token,
                request,
            )
            .await;
        }
        result
    }
}

impl ReqwestProviderClient {
    async fn bearer_token(&self, profile: &ProfileConfig, token: &str) -> Result<String, AppError> {
        match profile.provider {
            ProviderKind::GigaChat => {
                gigachat::bearer_token(
                    &self.client,
                    token,
                    &self.gigachat_tokens,
                    &self.gigachat_oauth_url,
                )
                .await
            }
            _ => Ok(token.to_string()),
        }
    }

    async fn refresh_bearer_token(
        &self,
        profile: &ProfileConfig,
        token: &str,
    ) -> Result<String, AppError> {
        match profile.provider {
            ProviderKind::GigaChat => {
                let key = gigachat::cache_key(token);
                self.gigachat_tokens.invalidate(&key);
                gigachat::refresh_bearer_token(
                    &self.client,
                    token,
                    &self.gigachat_tokens,
                    &self.gigachat_oauth_url,
                )
                .await
            }
            _ => Ok(token.to_string()),
        }
    }

    fn should_refresh_gigachat_token<T>(
        &self,
        profile: &ProfileConfig,
        stored_token: &str,
        result: &Result<T, AppError>,
    ) -> bool {
        profile.provider == ProviderKind::GigaChat
            && !gigachat::looks_like_access_token(stored_token)
            && matches!(
                result,
                Err(AppError::ProviderHttp(error))
                    if error.status == Some(reqwest::StatusCode::UNAUTHORIZED)
            )
    }

    pub async fn list_model_info(
        &self,
        profile: &ProfileConfig,
        token: &str,
    ) -> Result<Vec<ModelInfo>, AppError> {
        let spec = profile.provider.spec();
        let access_token = self.bearer_token(profile, token).await?;
        let result =
            openai_compatible::list_model_info(&self.client, spec, profile, &access_token).await;
        if self.should_refresh_gigachat_token(profile, token, &result) {
            let access_token = self.refresh_bearer_token(profile, token).await?;
            return openai_compatible::list_model_info(&self.client, spec, profile, &access_token)
                .await;
        }
        result
    }

    pub async fn chat_completion_with_debug(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: ChatRequest,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let spec = profile.provider.spec();
        let access_token = self.bearer_token(profile, token).await?;
        let result = openai_compatible::chat_completion_with_debug(
            &self.client,
            spec,
            profile,
            &access_token,
            request.clone(),
        )
        .await;
        if self.should_refresh_gigachat_token(profile, token, &result) {
            let access_token = self.refresh_bearer_token(profile, token).await?;
            return openai_compatible::chat_completion_with_debug(
                &self.client,
                spec,
                profile,
                &access_token,
                request,
            )
            .await;
        }
        result
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
            pricing: None,
            billing: None,
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
                max_completion_tokens: Some(128),
                temperature: Some(0.3),
                top_p: Some(0.9),
                top_k: Some(40),
                min_p: Some(0.05),
                top_a: Some(0.2),
                presence_penalty: Some(0.1),
                frequency_penalty: Some(0.2),
                repetition_penalty: Some(1.1),
                seed: Some(42),
                reasoning_effort: Some("high".to_string()),
                include_reasoning: Some(true),
                verbosity: Some("low".to_string()),
                logprobs: Some(true),
                top_logprobs: Some(3),
                n: Some(1),
                store: Some(false),
                parallel_tool_calls: Some(true),
                user: Some("test-user".to_string()),
                service_tier: Some("auto".to_string()),
                extra_params: serde_json::Map::from_iter([(
                    "custom_provider_flag".to_string(),
                    serde_json::Value::Bool(true),
                )]),
                stop: vec!["END".to_string()],
                answer_prefix: Some("Artem,".to_string()),
                answer_suffix: None,
                address_as: None,
                quote_question: true,
                format_instruction: None,
                completion_instruction: Some("Stop after the summary field.".to_string()),
            },
            pricing: None,
            billing: None,
        });

        assert_eq!(payload.max_tokens, None);
        assert_eq!(payload.max_completion_tokens, Some(128));
        assert_eq!(payload.temperature, Some(0.3));
        assert_eq!(payload.top_p, Some(0.9));
        assert_eq!(payload.top_k, Some(40));
        assert_eq!(payload.min_p, Some(0.05));
        assert_eq!(payload.top_a, Some(0.2));
        assert_eq!(payload.presence_penalty, Some(0.1));
        assert_eq!(payload.frequency_penalty, Some(0.2));
        assert_eq!(payload.repetition_penalty, Some(1.1));
        assert_eq!(payload.seed, Some(42));
        assert_eq!(payload.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(payload.reasoning, None);
        assert_eq!(payload.include_reasoning, None);
        assert_eq!(payload.verbosity.as_deref(), Some("low"));
        assert_eq!(payload.logprobs, Some(true));
        assert_eq!(payload.top_logprobs, Some(3));
        assert_eq!(payload.n, Some(1));
        assert_eq!(payload.store, Some(false));
        assert_eq!(payload.parallel_tool_calls, Some(true));
        assert_eq!(payload.user.as_deref(), Some("test-user"));
        assert_eq!(payload.service_tier.as_deref(), Some("auto"));
        assert_eq!(
            payload.extra_params.get("custom_provider_flag"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(payload.stop, vec!["END"]);
        assert_eq!(payload.response_format.expect("format").kind, "json_object");
        assert_eq!(payload.messages[0].role, Role::System);
        assert!(payload.messages[0].content.contains("bullet points"));
        assert!(payload.messages[0].content.contains("Artem"));
        assert!(payload.messages[1].content.contains("valid JSON object"));
        assert_eq!(payload.messages[3].role, Role::User);
    }

    #[test]
    fn openai_payload_converts_legacy_max_tokens_to_max_completion_tokens() {
        let payload = openai_compatible::chat_payload(ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "hello".to_string(),
            }],
            control: ResponseControl {
                max_tokens: Some(64),
                ..ResponseControl::uncontrolled()
            },
            pricing: None,
            billing: None,
        });

        assert_eq!(payload.max_tokens, None);
        assert_eq!(payload.max_completion_tokens, Some(64));
    }

    #[test]
    fn non_openai_payload_keeps_legacy_max_tokens() {
        let payload = openai_compatible::chat_payload_for_provider(
            ProviderKind::OpenRouter,
            ChatRequest {
                model: "m".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl {
                    max_tokens: Some(64),
                    ..ResponseControl::uncontrolled()
                },
                pricing: None,
                billing: None,
            },
        );

        assert_eq!(payload.max_tokens, Some(64));
        assert_eq!(payload.max_completion_tokens, None);
    }

    #[test]
    fn openrouter_payload_includes_reasoning_object() {
        let payload = openai_compatible::chat_payload_for_provider(
            ProviderKind::OpenRouter,
            ChatRequest {
                model: "m".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "think".to_string(),
                }],
                control: ResponseControl {
                    reasoning_effort: Some("high".to_string()),
                    include_reasoning: Some(true),
                    ..ResponseControl::uncontrolled()
                },
                pricing: None,
                billing: None,
            },
        );

        assert_eq!(
            payload
                .reasoning
                .as_ref()
                .map(|reasoning| reasoning.effort.as_str()),
            Some("high")
        );
        assert_eq!(payload.include_reasoning, Some(true));
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
