use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use reqwest::{StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};

use crate::{
    config::ProfileConfig,
    errors::{AppError, EndpointCategory, HttpProblem, ProviderHttpError, map_http_status},
    providers::{
        AuthScheme, BillingLookup, BillingProvider, ChatMessage, ChatRequest, ChatResponse,
        CostSource, HttpDebugRequest, HttpDebugResponse, ModelInfo, ModelPricing,
        ProviderExchangeDebug, ProviderKind, ProviderSpec, RequestCost, RequestMetrics,
        ResponseControl, ResponseFormat, StaticHeader, TokenUsage,
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
    Ok(list_model_info(client, spec, profile, token)
        .await?
        .into_iter()
        .map(|model| model.id)
        .collect())
}

pub async fn list_model_info(
    client: &Client,
    spec: ProviderSpec,
    profile: &ProfileConfig,
    token: &str,
) -> Result<Vec<ModelInfo>, AppError> {
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
    let pricing = request.pricing.clone();
    let billing = request.billing.clone();
    let billing_window_start = unix_seconds().saturating_sub(60);
    let billing_before =
        billing_cost_before_request(client, billing.as_ref(), billing_window_start)
            .await
            .ok()
            .flatten();
    let started = Instant::now();
    let request = authorized(client.post(url), spec, token)
        .json(&chat_payload_for_provider(spec.kind, request))
        .send();
    let response = request
        .await
        .map_err(|error| map_network_error(spec, EndpointCategory::Chat, error))?;
    let raw = response_text_or_error(response, spec, EndpointCategory::Chat).await?;
    let mut parsed =
        parse_chat_response(spec, &raw, started.elapsed().as_millis(), pricing.as_ref())?;
    apply_billing_cost(
        client,
        billing.as_ref(),
        billing_window_start,
        billing_before,
        &mut parsed,
    )
    .await?;
    Ok(parsed)
}

pub async fn chat_completion_with_debug(
    client: &Client,
    spec: ProviderSpec,
    profile: &ProfileConfig,
    token: &str,
    request: ChatRequest,
) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
    let url = endpoint(&profile.base_url, "chat/completions");
    let pricing = request.pricing.clone();
    let billing = request.billing.clone();
    let billing_window_start = unix_seconds().saturating_sub(60);
    let billing_before =
        billing_cost_before_request(client, billing.as_ref(), billing_window_start)
            .await
            .ok()
            .flatten();
    let body = serde_json::to_value(chat_payload_for_provider(spec.kind, request))
        .map_err(|error| AppError::Json(error.to_string()))?;
    let provider_request = HttpDebugRequest {
        method: "POST".to_string(),
        url: url.clone(),
        headers: debug_request_headers(spec),
        body: body.clone(),
    };
    let started = Instant::now();
    let response = authorized(client.post(url), spec, token)
        .json(&body)
        .send()
        .await
        .map_err(|error| map_network_error(spec, EndpointCategory::Chat, error))?;
    let status = response.status();
    let headers = debug_response_headers(response.headers());
    let raw = response
        .text()
        .await
        .map_err(|error| map_network_error(spec, EndpointCategory::Chat, error))?;
    let elapsed_ms = started.elapsed().as_millis();
    if !status.is_success() {
        return Err(AppError::ProviderHttp(map_http_status(
            spec.kind.to_string(),
            EndpointCategory::Chat,
            status,
            headers.get("retry-after").cloned(),
            short_reason(&raw),
        )));
    }
    let mut parsed = parse_chat_response(spec, &raw, elapsed_ms, pricing.as_ref())?;
    apply_billing_cost(
        client,
        billing.as_ref(),
        billing_window_start,
        billing_before,
        &mut parsed,
    )
    .await?;
    let provider_response = HttpDebugResponse {
        status: status.as_u16(),
        headers,
        body: parse_json_or_raw(&raw),
    };
    Ok((
        parsed,
        ProviderExchangeDebug {
            request: provider_request,
            response: provider_response,
        },
    ))
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

fn debug_request_headers(spec: ProviderSpec) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("authorization".to_string(), "Bearer [redacted]".to_string());
    headers.insert("content-type".to_string(), "application/json".to_string());
    for header in spec.extra_headers {
        headers.insert(header.name.to_ascii_lowercase(), header.value.to_string());
    }
    headers
}

fn debug_response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn parse_json_or_raw(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({ "raw": raw }))
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
    let body = response
        .text()
        .await
        .map_err(|error| map_network_error(spec, endpoint.clone(), error))?;
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
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_a: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenAiReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(flatten)]
    pub extra_params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiReasoning {
    pub effort: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoiceMessage {
    content: Option<String>,
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
    cost: Option<f64>,
    prompt_cache_hit_tokens: Option<u32>,
    prompt_cache_miss_tokens: Option<u32>,
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    pricing: Option<ModelEntryPricing>,
}

#[derive(Debug, Deserialize)]
struct ModelEntryPricing {
    prompt: Option<serde_json::Value>,
    completion: Option<serde_json::Value>,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCostsResponse {
    data: Vec<OpenAiCostsBucket>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCostsBucket {
    results: Vec<OpenAiCostsResult>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCostsResult {
    amount: Option<OpenAiCostAmount>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCostAmount {
    currency: Option<String>,
    value: Option<f64>,
}

pub fn chat_payload(request: ChatRequest) -> OpenAiChatPayload {
    chat_payload_for_provider(ProviderKind::OpenAiCompatible, request)
}

pub fn chat_payload_for_provider(
    provider: ProviderKind,
    request: ChatRequest,
) -> OpenAiChatPayload {
    let response_format = api_response_format(request.control.format);
    let control = request.control;
    let openrouter_reasoning = matches!(provider, ProviderKind::OpenRouter)
        .then(|| {
            control
                .reasoning_effort
                .as_ref()
                .map(|effort| OpenAiReasoning {
                    effort: effort.clone(),
                })
        })
        .flatten();
    OpenAiChatPayload {
        model: request.model,
        messages: controlled_messages(request.messages, &control),
        response_format,
        max_tokens: control.max_tokens,
        max_completion_tokens: control.max_completion_tokens,
        temperature: control.temperature,
        top_p: control.top_p,
        top_k: control.top_k,
        min_p: control.min_p,
        top_a: control.top_a,
        presence_penalty: control.presence_penalty,
        frequency_penalty: control.frequency_penalty,
        repetition_penalty: control.repetition_penalty,
        seed: control.seed,
        reasoning_effort: control.reasoning_effort.clone(),
        reasoning: openrouter_reasoning,
        include_reasoning: matches!(provider, ProviderKind::OpenRouter)
            .then_some(control.include_reasoning)
            .flatten(),
        verbosity: control.verbosity,
        logprobs: control.logprobs,
        top_logprobs: control.top_logprobs,
        n: control.n,
        store: control.store,
        parallel_tool_calls: control.parallel_tool_calls,
        user: control.user,
        service_tier: control.service_tier,
        stop: control.stop,
        extra_params: control.extra_params,
    }
}

fn api_response_format(format: ResponseFormat) -> Option<OpenAiResponseFormat> {
    match format {
        ResponseFormat::Text | ResponseFormat::Toon => None,
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

fn parse_chat_response(
    spec: ProviderSpec,
    raw: &str,
    elapsed_ms: u128,
    pricing: Option<&ModelPricing>,
) -> Result<ChatResponse, AppError> {
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
    let content = choice
        .message
        .content
        .filter(|content| !content.is_empty())
        .or(choice.message.reasoning_content)
        .unwrap_or_default();
    Ok(ChatResponse {
        text: content,
        finish_reason: choice.finish_reason,
        metrics: RequestMetrics {
            elapsed_ms,
            usage: parsed.usage.as_ref().map(token_usage),
            cost: parsed.usage.and_then(|usage| request_cost(usage, pricing)),
        },
    })
}

fn token_usage(usage: &OpenAiUsage) -> TokenUsage {
    let input_tokens = usage.prompt_tokens.unwrap_or_default();
    let output_tokens = usage.completion_tokens.unwrap_or_default();
    TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens: usage
            .total_tokens
            .unwrap_or_else(|| input_tokens.saturating_add(output_tokens)),
        cache_hit_input_tokens: cache_hit_input_tokens(usage),
        cache_miss_input_tokens: usage.prompt_cache_miss_tokens,
    }
}

fn cache_hit_input_tokens(usage: &OpenAiUsage) -> Option<u32> {
    usage
        .prompt_cache_hit_tokens
        .or_else(|| usage.prompt_tokens_details.as_ref()?.cached_tokens)
}

fn request_cost(usage: OpenAiUsage, pricing: Option<&ModelPricing>) -> Option<RequestCost> {
    if let Some(amount) = usage.cost {
        return Some(RequestCost {
            amount,
            currency: "credits".to_string(),
            source: CostSource::ProviderReported,
        });
    }
    configured_cost(&usage, pricing?)
}

fn configured_cost(usage: &OpenAiUsage, pricing: &ModelPricing) -> Option<RequestCost> {
    let input_tokens = usage.prompt_tokens?;
    let output_tokens = usage.completion_tokens.unwrap_or_default();
    let input_cost = match (
        cache_hit_input_tokens(usage),
        usage.prompt_cache_miss_tokens,
        pricing.cache_hit_input_per_million,
        pricing.cache_miss_input_per_million,
    ) {
        (Some(hit), Some(miss), Some(hit_price), Some(miss_price)) => {
            token_cost(hit, hit_price) + token_cost(miss, miss_price)
        }
        _ => token_cost(input_tokens, pricing.input_per_million.unwrap_or(0.0)),
    };
    let output_cost = token_cost(output_tokens, pricing.output_per_million);
    Some(RequestCost {
        amount: input_cost + output_cost,
        currency: pricing.currency.clone(),
        source: CostSource::ConfiguredPricing,
    })
}

fn token_cost(tokens: u32, price_per_million: f64) -> f64 {
    f64::from(tokens) * price_per_million / 1_000_000.0
}

async fn billing_cost_before_request(
    client: &Client,
    billing: Option<&BillingLookup>,
    window_start: u64,
) -> Result<Option<RequestCost>, AppError> {
    let Some(billing) = billing else {
        return Ok(None);
    };
    match billing.provider {
        BillingProvider::OpenAiCosts => openai_costs_total(client, billing, window_start).await,
    }
}

async fn apply_billing_cost(
    client: &Client,
    billing: Option<&BillingLookup>,
    window_start: u64,
    before: Option<RequestCost>,
    response: &mut ChatResponse,
) -> Result<(), AppError> {
    if matches!(
        response.metrics.cost.as_ref().map(|cost| &cost.source),
        Some(CostSource::ProviderReported)
    ) {
        return Ok(());
    }
    let Some(billing) = billing else {
        return Ok(());
    };
    match billing.provider {
        BillingProvider::OpenAiCosts => {
            let before_amount = before.as_ref().map(|cost| cost.amount).unwrap_or_default();
            let deadline = Instant::now() + Duration::from_secs(billing.poll_seconds);
            loop {
                if let Some(after) = openai_costs_total(client, billing, window_start).await? {
                    let delta = after.amount - before_amount;
                    if delta > 0.0 || Instant::now() >= deadline {
                        if delta > 0.0 {
                            response.metrics.cost = Some(RequestCost {
                                amount: delta,
                                currency: after.currency,
                                source: CostSource::BillingApi,
                            });
                        }
                        return Ok(());
                    }
                }
                if Instant::now() >= deadline {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn openai_costs_total(
    client: &Client,
    billing: &BillingLookup,
    window_start: u64,
) -> Result<Option<RequestCost>, AppError> {
    let start_time = window_start.to_string();
    let end_time = unix_seconds().saturating_add(120).to_string();
    let response = client
        .get("https://api.openai.com/v1/organization/costs")
        .bearer_auth(&billing.admin_token)
        .query(&[
            ("start_time", start_time.as_str()),
            ("end_time", end_time.as_str()),
            ("bucket_width", "1m"),
        ])
        .send()
        .await
        .map_err(|error| map_network_error(spec(), EndpointCategory::Chat, error))?;
    let raw = response_text_or_error(response, spec(), EndpointCategory::Chat).await?;
    parse_openai_costs_total(&raw)
}

fn parse_openai_costs_total(raw: &str) -> Result<Option<RequestCost>, AppError> {
    let parsed: OpenAiCostsResponse = serde_json::from_str(raw)
        .map_err(|error| AppError::Json(format!("invalid OpenAI costs response: {error}")))?;
    let mut currency = None;
    let amount = parsed
        .data
        .iter()
        .flat_map(|bucket| bucket.results.iter())
        .filter_map(|result| result.amount.as_ref())
        .filter_map(|amount| {
            if currency.is_none() {
                currency = amount.currency.clone();
            }
            amount.value
        })
        .sum::<f64>();
    Ok(Some(RequestCost {
        amount,
        currency: currency.unwrap_or_else(|| "usd".to_string()),
        source: CostSource::BillingApi,
    }))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn parse_models_response(spec: ProviderSpec, raw: &str) -> Result<Vec<ModelInfo>, AppError> {
    let parsed: ModelsResponse = serde_json::from_str(raw).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: spec.kind.to_string(),
            endpoint: EndpointCategory::Models,
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    Ok(parsed
        .data
        .into_iter()
        .map(|model| ModelInfo {
            pricing: model.pricing.and_then(model_pricing_from_entry),
            id: model.id,
        })
        .collect())
}

fn model_pricing_from_entry(pricing: ModelEntryPricing) -> Option<ModelPricing> {
    let input = pricing.prompt.or(pricing.input);
    let output = pricing.completion.or(pricing.output)?;
    Some(ModelPricing {
        currency: pricing.currency.unwrap_or_else(|| "USD".to_string()),
        input_per_million: input
            .and_then(|v| parse_price_per_token(&v))
            .map(|p| p * 1_000_000.0),
        output_per_million: parse_price_per_token(&output)? * 1_000_000.0,
        cache_hit_input_per_million: None,
        cache_miss_input_per_million: None,
    })
}

fn parse_price_per_token(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_response_parses_provider_reported_metrics() {
        let raw = r#"{
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "total_tokens": 14,
                "cost": 0.00014
            }
        }"#;

        let response = parse_chat_response(spec(), raw, 123, None).expect("parse response");

        assert_eq!(response.text, "hello");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
        assert_eq!(response.metrics.elapsed_ms, 123);
        assert_eq!(
            response.metrics.usage,
            Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                total_tokens: 14,
                cache_hit_input_tokens: None,
                cache_miss_input_tokens: None,
            })
        );
        let cost = response.metrics.cost.expect("provider-reported cost");
        assert_eq!(cost.amount, 0.00014);
        assert_eq!(cost.currency, "credits");
        assert_eq!(cost.source, CostSource::ProviderReported);
    }

    #[test]
    fn chat_response_does_not_guess_cost_without_provider_cost() {
        let raw = r#"{
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "total_tokens": 14
            }
        }"#;

        let response = parse_chat_response(spec(), raw, 123, None).expect("parse response");

        assert_eq!(response.metrics.cost, None);
    }

    #[test]
    fn chat_response_calculates_cost_from_configured_pricing() {
        let raw = r#"{
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "total_tokens": 1500
            }
        }"#;
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(2.0),
            output_per_million: 10.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        };

        let response =
            parse_chat_response(spec(), raw, 123, Some(&pricing)).expect("parse response");

        let cost = response.metrics.cost.expect("configured cost");
        assert!((cost.amount - 0.007).abs() < f64::EPSILON);
        assert_eq!(cost.currency, "USD");
        assert_eq!(cost.source, CostSource::ConfiguredPricing);
    }

    #[test]
    fn chat_response_uses_cache_pricing_when_usage_has_cache_breakdown() {
        let raw = r#"{
            "choices": [{
                "message": { "content": "hello" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "total_tokens": 1500,
                "prompt_cache_hit_tokens": 800,
                "prompt_cache_miss_tokens": 200
            }
        }"#;
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(2.0),
            output_per_million: 10.0,
            cache_hit_input_per_million: Some(0.2),
            cache_miss_input_per_million: Some(2.0),
        };

        let response =
            parse_chat_response(spec(), raw, 123, Some(&pricing)).expect("parse response");

        assert_eq!(
            response
                .metrics
                .usage
                .expect("usage")
                .cache_hit_input_tokens,
            Some(800)
        );
        let cost = response.metrics.cost.expect("configured cost");
        assert!((cost.amount - 0.00556).abs() < f64::EPSILON);
    }

    #[test]
    fn models_response_parses_pricing_metadata() {
        let raw = r#"{
            "data": [{
                "id": "provider/model",
                "pricing": {
                    "prompt": "0.0000007",
                    "completion": 0.0000021,
                    "currency": "USD"
                }
            }]
        }"#;

        let models = parse_models_response(spec(), raw).expect("parse models");

        assert_eq!(models[0].id, "provider/model");
        let pricing = models[0].pricing.as_ref().expect("pricing");
        assert!((pricing.input_per_million.unwrap_or(0.0) - 0.7).abs() < f64::EPSILON);
        assert!((pricing.output_per_million - 2.1).abs() < 0.0000000001);
    }

    /// Баг 2: расчёт стоимости без цены за input (DeepSeek) — считается только output
    #[test]
    fn configured_cost_with_no_input_price_charges_only_output() {
        let raw = r#"{
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
        }"#;
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            input_per_million: None,
            output_per_million: 4.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        };

        let response = parse_chat_response(spec(), raw, 1, Some(&pricing)).expect("parse");

        let cost = response
            .metrics
            .cost
            .expect("должна считаться стоимость без input цены");
        // 500 * 4.0 / 1_000_000 = 0.002
        assert!(
            (cost.amount - 0.002).abs() < 1e-10,
            "actual: {}",
            cost.amount
        );
        assert_eq!(cost.source, CostSource::ConfiguredPricing);
    }

    /// Баг 2: ModelPricing из entry без поля input — pricing должен возвращаться (не None)
    #[test]
    fn model_pricing_from_entry_with_no_input_field_still_returns_some() {
        let raw = r#"{
            "data": [{
                "id": "free-model",
                "pricing": { "completion": "0.000001" }
            }]
        }"#;

        let models = parse_models_response(spec(), raw).expect("parse");

        let pricing = models[0]
            .pricing
            .as_ref()
            .expect("pricing должен быть Some даже без поля input");
        assert!(
            pricing.input_per_million.is_none(),
            "input_per_million должен быть None"
        );
        assert!((pricing.output_per_million - 1.0).abs() < f64::EPSILON);
    }

    /// Провайдер возвращает стоимость — она имеет приоритет над configured pricing
    #[test]
    fn provider_reported_cost_takes_priority_over_configured_pricing() {
        let raw = r#"{
            "choices": [{ "message": { "content": "ok" } }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 100,
                "total_tokens": 200,
                "cost": 0.042
            }
        }"#;
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(999.0),
            output_per_million: 999.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        };

        let response = parse_chat_response(spec(), raw, 1, Some(&pricing)).expect("parse");

        let cost = response.metrics.cost.expect("cost");
        assert_eq!(cost.amount, 0.042);
        assert_eq!(cost.source, CostSource::ProviderReported);
    }

    /// Без usage нет стоимости
    #[test]
    fn no_usage_in_response_means_no_cost() {
        let raw = r#"{ "choices": [{ "message": { "content": "ok" } }] }"#;
        let pricing = ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(2.0),
            output_per_million: 10.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        };

        let response = parse_chat_response(spec(), raw, 1, Some(&pricing)).expect("parse");

        assert!(response.metrics.cost.is_none());
        assert!(response.metrics.usage.is_none());
    }
}
