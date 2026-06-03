use reqwest::Client;
use reqwest::{StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};

use crate::{
    config::ProfileConfig,
    errors::{AppError, EndpointCategory, HttpProblem, ProviderHttpError, map_http_status},
    providers::{
        AuthScheme, ChatMessage, ChatRequest, ChatResponse, ProviderKind, ProviderSpec,
        ResponseControl, ResponseFormat, StaticHeader,
    },
};

const EXTRA_HEADERS: &[StaticHeader] = &[];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::OpenAiCompatible,
        display_name: "OpenAI-compatible",
        default_base_url: "https://api.openai.com/v1",
        default_model: "gpt-4.1-mini",
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}

pub async fn list_models(
    client: &Client,
    spec: ProviderSpec,
    profile: &ProfileConfig,
    token: &str,
) -> Result<Vec<String>, AppError> {
    let url = endpoint(&profile.base_url, "models");
    let request = authorized(client.get(url), spec, token);
    let response = request
        .send()
        .await
        .map_err(|error| map_network_error(spec, EndpointCategory::Models, error))?;
    let raw = response_text_or_error(response, spec, EndpointCategory::Models).await?;
    parse_models_response(spec, &raw)
}

pub async fn chat_completion(
    client: &Client,
    spec: ProviderSpec,
    profile: &ProfileConfig,
    token: &str,
    request: ChatRequest,
) -> Result<ChatResponse, AppError> {
    let url = endpoint(&profile.base_url, "chat/completions");
    let request = authorized(client.post(url), spec, token)
        .json(&chat_payload(request))
        .send();
    let response = request
        .await
        .map_err(|error| map_network_error(spec, EndpointCategory::Chat, error))?;
    let raw = response_text_or_error(response, spec, EndpointCategory::Chat).await?;
    parse_chat_response(spec, &raw)
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path)
}

fn authorized(
    request: reqwest::RequestBuilder,
    spec: ProviderSpec,
    token: &str,
) -> reqwest::RequestBuilder {
    let request = match spec.auth_scheme {
        AuthScheme::Bearer => request.bearer_auth(token),
    };
    spec.extra_headers.iter().fold(request, |request, header| {
        request.header(header.name, header.value)
    })
}

async fn response_text_or_error(
    response: reqwest::Response,
    spec: ProviderSpec,
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
        spec.kind.to_string(),
        endpoint,
        status,
        retry_after,
        short_reason(&body),
    )))
}

fn map_network_error(
    spec: ProviderSpec,
    endpoint: EndpointCategory,
    error: reqwest::Error,
) -> AppError {
    AppError::ProviderHttp(ProviderHttpError {
        provider: spec.kind.to_string(),
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

#[derive(Debug, Serialize, PartialEq)]
pub struct OpenAiChatPayload {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenAiResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
    finish_reason: Option<String>,
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
    let response_format = api_response_format(request.control.format);
    let control = request.control;
    OpenAiChatPayload {
        model: request.model,
        messages: controlled_messages(request.messages, &control),
        response_format,
        max_tokens: control.max_tokens,
        temperature: control.temperature,
        top_p: control.top_p,
        presence_penalty: control.presence_penalty,
        frequency_penalty: control.frequency_penalty,
        seed: control.seed,
        stop: control.stop,
    }
}

fn api_response_format(format: ResponseFormat) -> Option<OpenAiResponseFormat> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(OpenAiResponseFormat {
            kind: "json_object",
        }),
    }
}

fn controlled_messages(messages: Vec<ChatMessage>, control: &ResponseControl) -> Vec<ChatMessage> {
    let mut controlled = control.instruction_messages();
    controlled.extend(messages);
    controlled
}

fn parse_chat_response(spec: ProviderSpec, raw: &str) -> Result<ChatResponse, AppError> {
    let parsed: OpenAiChatResponse = serde_json::from_str(raw).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: spec.kind.to_string(),
            endpoint: EndpointCategory::Chat,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    let choice = parsed.choices.into_iter().next().ok_or_else(|| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: spec.kind.to_string(),
            endpoint: EndpointCategory::Chat,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: "missing choices[0].message.content".to_string(),
        })
    })?;
    if choice.message.content.is_empty() {
        return Err(AppError::ProviderHttp(ProviderHttpError {
            provider: spec.kind.to_string(),
            endpoint: EndpointCategory::Chat,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: "empty choices[0].message.content".to_string(),
        }));
    }
    Ok(ChatResponse {
        text: choice.message.content,
        finish_reason: choice.finish_reason,
    })
}

fn parse_models_response(spec: ProviderSpec, raw: &str) -> Result<Vec<String>, AppError> {
    let parsed: ModelsResponse = serde_json::from_str(raw).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: spec.kind.to_string(),
            endpoint: EndpointCategory::Models,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    Ok(parsed.data.into_iter().map(|model| model.id).collect())
}
