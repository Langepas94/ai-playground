use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    config::{AppConfig, ProfileConfig, token_ref},
    errors::AppError,
    providers::{
        AnswerFormat, BillingLookup, BillingProvider, ChatMessage, ChatRequest, HttpDebugRequest,
        HttpDebugResponse, ModelPricing, ProviderKind, ReqwestProviderClient, ResponseControl,
        ResponseFormat, Role, validate_base_url,
    },
    secrets::{KeyringSecretStore, SecretStore, get_config_profile_token, set_profile_token},
};

#[derive(Clone)]
struct AppState {
    client: ReqwestProviderClient,
    secrets: Arc<dyn SecretStore>,
}

pub async fn serve(addr: SocketAddr) -> Result<(), AppError> {
    let client = ReqwestProviderClient::new()?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let app = Router::new()
        .route("/", get(index))
        .route("/api/providers", get(providers))
        .route("/api/models", post(models))
        .route("/api/chat", post(chat))
        .with_state(AppState { client, secrets });
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    println!(
        "Web UI: http://{}",
        listener.local_addr().map_err(|error| {
            AppError::Terminal(format!("could not read listener address: {error}"))
        })?
    );
    axum::serve(listener, app)
        .await
        .map_err(|error| AppError::Terminal(error.to_string()))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn providers() -> Json<ProvidersResponse> {
    Json(ProvidersResponse {
        providers: ProviderKind::all()
            .iter()
            .map(|provider| {
                let spec = provider.spec();
                ProviderView {
                    id: provider.to_string(),
                    name: spec.display_name.to_string(),
                    default_base_url: spec.default_base_url.to_string(),
                    default_model: spec.default_model.to_string(),
                    parameter_constraints: parameter_constraints(*provider),
                }
            })
            .collect(),
    })
}

async fn models(
    State(state): State<AppState>,
    Json(request): Json<ModelsRequest>,
) -> Result<Json<ModelsResponse>, WebError> {
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    let token = resolve_web_token(
        state.secrets.as_ref(),
        &profile,
        &request.token,
        request.token_provider.as_deref(),
    )?;
    let mut models = state.client.list_model_info(&profile, &token).await?;
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(Json(ModelsResponse {
        models: models.into_iter().map(ModelView::from).collect(),
    }))
}

async fn chat(
    State(state): State<AppState>,
    Json(request): Json<ChatWebRequest>,
) -> Result<Json<ChatWebResponse>, WebError> {
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    if request.prompt.trim().is_empty() {
        return Err(AppError::InvalidInput("Prompt is required".to_string()).into());
    }
    let token = resolve_web_token(
        state.secrets.as_ref(),
        &profile,
        &request.token,
        request.token_provider.as_deref(),
    )?;
    let control = request.control.clone().into_control();
    let chat_request = ChatRequest {
        model: profile.model.clone(),
        messages: request.chat_messages(),
        control,
        pricing: request
            .pricing
            .clone()
            .and_then(WebPricing::into_model_pricing),
        billing: request
            .billing
            .clone()
            .and_then(WebBilling::into_billing_lookup),
    };
    let (response, provider_debug) = state
        .client
        .chat_completion_with_debug(&profile, &token, chat_request)
        .await?;
    Ok(Json(ChatWebResponse {
        text: response.text,
        finish_reason: response.finish_reason,
        metrics: response.metrics,
        debug: ChatDebugView {
            provider_request: provider_debug.request,
            provider_response: provider_debug.response,
        },
    }))
}

#[derive(Debug, Serialize)]
struct ProvidersResponse {
    providers: Vec<ProviderView>,
}

#[derive(Debug, Serialize)]
struct ProviderView {
    id: String,
    name: String,
    default_base_url: String,
    default_model: String,
    parameter_constraints: Vec<ParameterConstraintView>,
}

#[derive(Debug, Serialize)]
struct ParameterConstraintView {
    id: &'static str,
    supported: bool,
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
    note: &'static str,
}

fn parameter_constraints(provider: ProviderKind) -> Vec<ParameterConstraintView> {
    let mut constraints = openrouter_like_constraints();
    match provider {
        ProviderKind::OpenRouter | ProviderKind::OpenAiCompatible => constraints,
        ProviderKind::DeepSeek => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxCompletionTokens",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "repetitionPenalty",
                    "n",
                    "store",
                    "parallelToolCalls",
                ],
                "DeepSeek docs: unsupported/deprecated; the API will ignore this parameter.",
            );
            constraints
        }
        ProviderKind::Kimi => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxTokens",
                    "temperature",
                    "topP",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "repetitionPenalty",
                    "n",
                    "store",
                    "parallelToolCalls",
                ],
                "Kimi current docs do not list this parameter for Chat Completion.",
            );
            constraints
        }
        ProviderKind::GigaChat => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxCompletionTokens",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "store",
                    "parallelToolCalls",
                ],
                "GigaChat docs do not list this parameter for Chat Completion.",
            );
            set_constraint(
                &mut constraints,
                "temperature",
                Some(0.01),
                None,
                "GigaChat docs: temperature must be > 0; values above 2 can be too random.",
            );
            constraints
        }
    }
}

fn openrouter_like_constraints() -> Vec<ParameterConstraintView> {
    vec![
        constraint("maxTokens", true, Some(1.0), None, Some(1.0), ">= 1"),
        constraint(
            "maxCompletionTokens",
            true,
            Some(1.0),
            None,
            Some(1.0),
            ">= 1",
        ),
        constraint("temperature", true, Some(0.0), Some(2.0), Some(0.1), "0..2"),
        constraint("topP", true, Some(0.0), Some(1.0), Some(0.05), "0..1"),
        constraint("topK", true, Some(0.0), None, Some(1.0), ">= 0"),
        constraint("minP", true, Some(0.0), Some(1.0), Some(0.01), "0..1"),
        constraint("topA", true, Some(0.0), Some(1.0), Some(0.01), "0..1"),
        constraint(
            "presencePenalty",
            true,
            Some(-2.0),
            Some(2.0),
            Some(0.1),
            "-2..2",
        ),
        constraint(
            "frequencyPenalty",
            true,
            Some(-2.0),
            Some(2.0),
            Some(0.1),
            "-2..2",
        ),
        constraint(
            "repetitionPenalty",
            true,
            Some(0.0),
            Some(2.0),
            Some(0.05),
            "0..2",
        ),
        constraint(
            "topLogprobs",
            true,
            Some(0.0),
            Some(20.0),
            Some(1.0),
            "0..20",
        ),
        constraint("n", true, Some(1.0), None, Some(1.0), ">= 1"),
        constraint("includeReasoning", true, None, None, None, "boolean"),
        constraint("logprobs", true, None, None, None, "boolean"),
        constraint("store", true, None, None, None, "boolean"),
        constraint(
            "parallelToolCalls",
            true,
            None,
            None,
            None,
            "Only send when tools are specified; OpenAI-compatible APIs reject it without tools.",
        ),
    ]
}

fn constraint(
    id: &'static str,
    supported: bool,
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
    note: &'static str,
) -> ParameterConstraintView {
    ParameterConstraintView {
        id,
        supported,
        min,
        max,
        step,
        note,
    }
}

fn mark_unsupported(constraints: &mut [ParameterConstraintView], ids: &[&str], note: &'static str) {
    for constraint in constraints {
        if ids.contains(&constraint.id) {
            constraint.supported = false;
            constraint.note = note;
        }
    }
}

fn set_constraint(
    constraints: &mut [ParameterConstraintView],
    id: &str,
    min: Option<f32>,
    max: Option<f32>,
    note: &'static str,
) {
    if let Some(constraint) = constraints
        .iter_mut()
        .find(|constraint| constraint.id == id)
    {
        constraint.min = min;
        constraint.max = max;
        constraint.note = note;
    }
}

#[derive(Debug, Deserialize)]
struct ModelsRequest {
    provider: String,
    base_url: String,
    token: String,
    token_provider: Option<String>,
}

impl ModelsRequest {
    fn profile(&self) -> Result<ProfileConfig, AppError> {
        let provider = parse_provider(&self.provider)?;
        Ok(ProfileConfig {
            provider,
            model: provider.default_model().to_string(),
            base_url: self.base_url.clone(),
            token_ref: String::new(),
        })
    }
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    models: Vec<ModelView>,
}

#[derive(Debug, Serialize)]
struct ModelView {
    id: String,
    pricing: Option<ModelPricing>,
}

impl From<crate::providers::ModelInfo> for ModelView {
    fn from(model: crate::providers::ModelInfo) -> Self {
        Self {
            id: model.id,
            pricing: model.pricing,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatWebRequest {
    provider: String,
    base_url: String,
    token: String,
    token_provider: Option<String>,
    model: String,
    system_prompt: Option<String>,
    prompt: String,
    control: WebResponseControl,
    pricing: Option<WebPricing>,
    billing: Option<WebBilling>,
}

impl ChatWebRequest {
    fn profile(&self) -> Result<ProfileConfig, AppError> {
        let provider = parse_provider(&self.provider)?;
        if self.model.trim().is_empty() {
            return Err(AppError::InvalidInput("Model is required".to_string()));
        }
        Ok(ProfileConfig {
            provider,
            model: self.model.trim().to_string(),
            base_url: self.base_url.clone(),
            token_ref: String::new(),
        })
    }

    fn chat_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        if let Some(system_prompt) = blank_to_none(self.system_prompt.clone()) {
            messages.push(ChatMessage {
                role: Role::System,
                content: system_prompt,
            });
        }
        messages.push(ChatMessage {
            role: Role::User,
            content: self.prompt.clone(),
        });
        messages
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WebResponseControl {
    response_format: Option<String>,
    answer_format: Option<String>,
    max_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    min_p: Option<f32>,
    top_a: Option<f32>,
    presence_penalty: Option<f32>,
    frequency_penalty: Option<f32>,
    repetition_penalty: Option<f32>,
    seed: Option<i64>,
    reasoning_effort: Option<String>,
    include_reasoning: Option<bool>,
    verbosity: Option<String>,
    logprobs: Option<bool>,
    top_logprobs: Option<u32>,
    n: Option<u32>,
    store: Option<bool>,
    parallel_tool_calls: Option<bool>,
    user: Option<String>,
    service_tier: Option<String>,
    extra_params: Option<serde_json::Map<String, serde_json::Value>>,
    stop: Vec<String>,
    answer_prefix: Option<String>,
    answer_suffix: Option<String>,
    address_as: Option<String>,
    quote_question: bool,
    format_instruction: Option<String>,
    completion_instruction: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WebPricing {
    input_per_million: Option<f64>,
    output_per_million: Option<f64>,
    cache_hit_input_per_million: Option<f64>,
    cache_miss_input_per_million: Option<f64>,
    currency: Option<String>,
}

impl WebPricing {
    fn into_model_pricing(self) -> Option<ModelPricing> {
        Some(ModelPricing {
            currency: blank_to_none(self.currency).unwrap_or_else(|| "USD".to_string()),
            input_per_million: self.input_per_million?,
            output_per_million: self.output_per_million?,
            cache_hit_input_per_million: self.cache_hit_input_per_million,
            cache_miss_input_per_million: self.cache_miss_input_per_million,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WebBilling {
    openai_admin_token: Option<String>,
    openai_cost_poll_seconds: Option<u64>,
}

impl WebBilling {
    fn into_billing_lookup(self) -> Option<BillingLookup> {
        let token = blank_to_none(self.openai_admin_token)?;
        Some(BillingLookup {
            provider: BillingProvider::OpenAiCosts,
            admin_token: token,
            poll_seconds: self.openai_cost_poll_seconds.unwrap_or(20),
        })
    }
}

impl WebResponseControl {
    fn into_control(self) -> ResponseControl {
        ResponseControl {
            format: match self.response_format.as_deref() {
                Some("json-object") => ResponseFormat::JsonObject,
                _ => ResponseFormat::Text,
            },
            answer_format: match self.answer_format.as_deref() {
                Some("bullets") => AnswerFormat::Bullets,
                Some("numbered") => AnswerFormat::Numbered,
                Some("short") => AnswerFormat::Short,
                Some("steps") => AnswerFormat::Steps,
                Some("table") => AnswerFormat::Table,
                _ => AnswerFormat::Natural,
            },
            max_tokens: self.max_tokens,
            max_completion_tokens: self.max_completion_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            min_p: self.min_p,
            top_a: self.top_a,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
            repetition_penalty: self.repetition_penalty,
            seed: self.seed,
            reasoning_effort: blank_to_none(self.reasoning_effort),
            include_reasoning: self.include_reasoning,
            verbosity: blank_to_none(self.verbosity),
            logprobs: self.logprobs,
            top_logprobs: self.top_logprobs,
            n: self.n,
            store: self.store,
            parallel_tool_calls: self.parallel_tool_calls,
            user: blank_to_none(self.user),
            service_tier: blank_to_none(self.service_tier),
            extra_params: self.extra_params.unwrap_or_default(),
            stop: self.stop,
            answer_prefix: blank_to_none(self.answer_prefix),
            answer_suffix: blank_to_none(self.answer_suffix),
            address_as: blank_to_none(self.address_as),
            quote_question: self.quote_question,
            format_instruction: blank_to_none(self.format_instruction),
            completion_instruction: blank_to_none(self.completion_instruction),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatWebResponse {
    text: String,
    finish_reason: Option<String>,
    metrics: crate::providers::RequestMetrics,
    debug: ChatDebugView,
}

#[derive(Debug, Serialize)]
struct ChatDebugView {
    provider_request: HttpDebugRequest,
    provider_response: HttpDebugResponse,
}

fn resolve_web_token(
    secrets: &dyn SecretStore,
    profile: &ProfileConfig,
    token_override: &str,
    token_provider: Option<&str>,
) -> Result<String, AppError> {
    let token_override = token_override.trim();
    if !token_override.is_empty() && token_override_belongs_to_provider(profile, token_provider)? {
        set_profile_token(secrets, profile, token_override)?;
        return Ok(token_override.to_string());
    }

    if let Some(token) = secrets.get_token(&token_ref(&profile.provider))? {
        return Ok(token);
    }

    let config = AppConfig::load()?;
    for (name, candidate) in &config.profiles {
        if candidate.provider != profile.provider {
            continue;
        }
        if let Some(token) = get_config_profile_token(secrets, &config, name, candidate)? {
            return Ok(token);
        }
    }

    Err(AppError::InvalidInput(
        "API token is required. Save it with `ai token set --profile <name>` or paste it once in the web UI.".to_string(),
    ))
}

fn token_override_belongs_to_provider(
    profile: &ProfileConfig,
    token_provider: Option<&str>,
) -> Result<bool, AppError> {
    let Some(token_provider) = token_provider else {
        return Ok(true);
    };
    let token_provider = token_provider.trim();
    if token_provider.is_empty() {
        return Ok(false);
    }
    Ok(parse_provider(token_provider)? == profile.provider)
}

fn parse_provider(value: &str) -> Result<ProviderKind, AppError> {
    value.parse()
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

struct WebError(AppError);

impl From<AppError> for WebError {
    fn from(error: AppError) -> Self {
        Self(error)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = match self.0 {
            AppError::InvalidInput(_) | AppError::InvalidBaseUrl { .. } => StatusCode::BAD_REQUEST,
            AppError::ProviderHttp(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Json(ErrorResponse {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ai playground web</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f7f8fb;
      --panel: #ffffff;
      --text: #172033;
      --muted: #5d687c;
      --line: #d9deea;
      --accent: #166f7a;
      --accent-dark: #0f5660;
      --danger: #a53b3b;
      --output: #111827;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: var(--bg);
      color: var(--text);
    }
    main {
      width: min(1480px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 24px 0 32px;
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 18px;
    }
    h1 {
      margin: 0;
      font-size: 24px;
      line-height: 1.15;
      letter-spacing: 0;
    }
    .status {
      min-height: 22px;
      color: var(--muted);
      font-size: 14px;
      text-align: right;
    }
    .layout {
      display: grid;
      grid-template-columns: minmax(300px, 380px) minmax(0, 1fr);
      gap: 16px;
      align-items: start;
    }
    section {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 16px;
    }
    .controls {
      display: grid;
      gap: 14px;
    }
    .group {
      display: grid;
      gap: 10px;
      padding-bottom: 14px;
      border-bottom: 1px solid var(--line);
    }
    .group:last-child {
      border-bottom: 0;
      padding-bottom: 0;
    }
    h2 {
      margin: 0;
      font-size: 15px;
      line-height: 1.2;
      letter-spacing: 0;
    }
    label {
      display: grid;
      gap: 5px;
      color: var(--muted);
      font-size: 13px;
      line-height: 1.3;
    }
    input, select, textarea, button {
      font: inherit;
    }
    input, select, textarea {
      width: 100%;
      border: 1px solid var(--line);
      border-radius: 6px;
      padding: 10px 11px;
      color: var(--text);
      background: #fff;
      min-height: 42px;
    }
    textarea {
      min-height: 84px;
      resize: vertical;
      line-height: 1.45;
    }
    #prompt {
      min-height: 180px;
      max-height: 34vh;
    }
    #systemPrompt {
      min-height: 64px;
      max-height: 18vh;
    }
    .row {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 10px;
    }
    .inline {
      display: flex;
      align-items: center;
      gap: 8px;
      color: var(--text);
    }
    .inline input {
      width: 18px;
      min-height: 18px;
      padding: 0;
    }
    .actions {
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
    }
    details.panel {
      border: 1px solid var(--line);
      border-radius: 8px;
      background: #fff;
    }
    details.panel summary {
      cursor: pointer;
      padding: 11px 12px;
      color: var(--text);
      font-weight: 650;
      list-style-position: inside;
    }
    details.panel .panel-body {
      display: grid;
      gap: 10px;
      padding: 0 12px 12px;
    }
    .compact-row {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 10px;
    }
    .send-row {
      display: flex;
      justify-content: space-between;
      gap: 10px;
      flex-wrap: wrap;
      align-items: center;
    }
    button {
      border: 1px solid transparent;
      border-radius: 6px;
      min-height: 42px;
      padding: 9px 13px;
      cursor: pointer;
      background: var(--accent);
      color: white;
      font-weight: 650;
    }
    button.secondary {
      background: #eef3f4;
      color: var(--accent-dark);
      border-color: #c8d6d9;
    }
    button:disabled {
      opacity: .6;
      cursor: wait;
    }
    pre {
      min-height: 320px;
      margin: 0;
      padding: 16px;
      overflow: auto;
      white-space: pre-wrap;
      word-break: break-word;
      background: var(--output);
      color: #f9fafb;
      border-radius: 8px;
      line-height: 1.5;
      font-size: 14px;
    }
    details.debug {
      margin-top: 12px;
      border: 1px solid var(--line);
      border-radius: 8px;
      background: #fff;
    }
    details.debug summary {
      cursor: pointer;
      padding: 12px 14px;
      color: var(--text);
      font-weight: 650;
    }
    .debug-grid {
      display: grid;
      gap: 10px;
      padding: 0 12px 12px;
    }
    .debug-item {
      display: grid;
      gap: 6px;
    }
    .debug-item h3 {
      margin: 0;
      color: var(--muted);
      font-size: 13px;
      line-height: 1.25;
      letter-spacing: 0;
    }
    .debug-item pre {
      min-height: 120px;
      max-height: 300px;
      border-radius: 6px;
      font-size: 12px;
    }
    .metrics {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 8px;
      margin: 12px 0 10px;
    }
    .metric {
      display: grid;
      gap: 3px;
      min-height: 58px;
      padding: 9px 10px;
      border: 1px solid var(--line);
      border-radius: 6px;
      background: #f9fbfc;
    }
    .metric span {
      color: var(--muted);
      font-size: 11px;
      line-height: 1.2;
      text-transform: uppercase;
    }
    .metric strong {
      color: var(--text);
      font-size: 14px;
      line-height: 1.25;
      font-weight: 700;
      word-break: break-word;
    }
    .metric-lines {
      display: grid;
      gap: 2px;
      color: var(--text);
      font-size: 13px;
      line-height: 1.25;
    }
    .metric-line {
      display: flex;
      justify-content: space-between;
      gap: 10px;
    }
    .metric-line em {
      color: var(--muted);
      font-style: normal;
    }
    .metric-line b {
      font-weight: 700;
    }
    .warnings {
      display: none;
      margin: 0;
      padding: 10px 12px;
      border: 1px solid #e5bf59;
      border-radius: 6px;
      background: #fff8db;
      color: #65460b;
      font-size: 13px;
      line-height: 1.35;
    }
    .warnings.visible { display: block; }
    .field-warning input,
    .field-warning select,
    .field-warning textarea {
      border-color: #c7861a;
      box-shadow: 0 0 0 2px rgba(199, 134, 26, .12);
    }
    .unsupported {
      opacity: .58;
    }
    .error { color: var(--danger); }
    @media (max-width: 900px) {
      main { width: min(100vw - 20px, 720px); padding-top: 14px; }
      header { align-items: flex-start; flex-direction: column; }
      .status { text-align: left; }
      .layout { grid-template-columns: 1fr; }
      .row { grid-template-columns: 1fr; }
      .metrics { grid-template-columns: 1fr; }
    }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>ai playground</h1>
      <div id="status" class="status">Готово</div>
    </header>

    <div class="layout">
      <section class="controls">
        <div class="group">
          <h2>Провайдер</h2>
          <label>Provider<select id="provider"></select></label>
          <label>Model<select id="model"></select></label>
          <div class="actions">
            <button id="loadModels" class="secondary" type="button">Загрузить модели</button>
          </div>
          <details class="panel">
            <summary>Подключение</summary>
            <div class="panel-body">
              <label>API token override<input id="token" type="password" autocomplete="off" spellcheck="false" placeholder="Пусто = взять из keychain"></label>
              <label>Base URL<input id="baseUrl" spellcheck="false"></label>
              <label>Custom model id<input id="customModel" spellcheck="false" placeholder="Если нужной модели нет в списке"></label>
            </div>
          </details>
        </div>

        <div class="group">
          <h2>Основные параметры</h2>
          <div id="parameterWarnings" class="warnings"></div>
          <div class="row">
            <label>max_tokens<input id="maxTokens" type="number" min="1" step="1" value="1024"></label>
            <label>temperature<input id="temperature" type="number" min="0" max="2" step="0.1" value="1"></label>
          </div>
          <div class="row">
            <label>top_p<input id="topP" type="number" min="0" max="1" step="0.05" value="1"></label>
            <label>answer_format<select id="answerFormat"><option value="natural">natural</option><option value="bullets">bullets</option><option value="numbered">numbered</option><option value="short">short</option><option value="steps">steps</option><option value="table">table</option></select></label>
          </div>
          <details class="panel">
            <summary>Цена</summary>
            <div class="panel-body">
              <div class="row">
                <label>input / 1M<input id="priceInput" type="number" min="0" step="0.000001" placeholder="Например 0.07"></label>
                <label>output / 1M<input id="priceOutput" type="number" min="0" step="0.000001" placeholder="Например 1.10"></label>
              </div>
              <div class="row">
                <label>cache hit input / 1M<input id="priceCacheHitInput" type="number" min="0" step="0.000001"></label>
                <label>cache miss input / 1M<input id="priceCacheMissInput" type="number" min="0" step="0.000001"></label>
              </div>
              <label>currency<input id="priceCurrency" value="USD" spellcheck="false"></label>
            </div>
          </details>
          <details class="panel">
            <summary>Billing API</summary>
            <div class="panel-body">
              <label>OpenAI Admin API key<input id="openaiAdminToken" type="password" autocomplete="off" spellcheck="false" placeholder="Для /organization/costs"></label>
              <label>poll seconds<input id="openaiCostPollSeconds" type="number" min="0" step="1" value="20"></label>
            </div>
          </details>
          <details class="panel">
            <summary>Расширенные параметры</summary>
            <div class="panel-body">
              <div class="row">
                <label>response_format<select id="responseFormat"><option value="text">text</option><option value="json-object">json_object</option></select></label>
                <label>max_completion_tokens<input id="maxCompletionTokens" type="number" min="1" step="1"></label>
              </div>
              <div class="row">
                <label>top_k<input id="topK" type="number" min="0" step="1"></label>
                <label>min_p<input id="minP" type="number" min="0" max="1" step="0.01"></label>
              </div>
              <div class="row">
                <label>top_a<input id="topA" type="number" min="0" max="1" step="0.01"></label>
                <label>seed<input id="seed" type="number" step="1"></label>
              </div>
              <div class="row">
                <label>presence_penalty<input id="presencePenalty" type="number" min="-2" max="2" step="0.1" value="0"></label>
                <label>frequency_penalty<input id="frequencyPenalty" type="number" min="-2" max="2" step="0.1" value="0"></label>
              </div>
              <label>repetition_penalty<input id="repetitionPenalty" type="number" min="0" step="0.05"></label>
              <label>stop sequences<textarea id="stop" placeholder="Одна stop sequence на строку"></textarea></label>
              <div class="row">
                <label>reasoning_effort<select id="reasoningEffort"><option value="" selected>provider default</option><option value="none">none</option><option value="minimal">minimal</option><option value="low">low</option><option value="medium">medium</option><option value="high">high</option><option value="xhigh">xhigh</option></select></label>
                <label>verbosity<select id="verbosity"><option value="" selected>provider default</option><option value="low">low</option><option value="medium">medium</option><option value="high">high</option></select></label>
              </div>
              <div class="row">
                <label>n<input id="n" type="number" min="1" step="1" value="1"></label>
                <label>top_logprobs<input id="topLogprobs" type="number" min="0" max="20" step="1"></label>
              </div>
              <div class="row">
                <label>service_tier<select id="serviceTier"><option value="">provider default</option><option value="auto">auto</option><option value="default">default</option><option value="flex">flex</option><option value="priority">priority</option></select></label>
                <label>user<input id="user" spellcheck="false"></label>
              </div>
              <label class="inline"><input id="includeReasoning" type="checkbox"> include_reasoning</label>
              <label class="inline"><input id="logprobs" type="checkbox"> logprobs</label>
              <label class="inline"><input id="store" type="checkbox"> store</label>
              <label class="inline"><input id="parallelToolCalls" type="checkbox"> parallel_tool_calls</label>
            </div>
          </details>
        </div>

        <div class="group">
          <details class="panel">
            <summary>Формат и JSON</summary>
            <div class="panel-body">
              <div class="row">
                <label>answer_prefix<input id="answerPrefix"></label>
                <label>answer_suffix<input id="answerSuffix"></label>
              </div>
              <label>address_as<input id="addressAs"></label>
              <label class="inline"><input id="quoteQuestion" type="checkbox"> quote_question</label>
              <label>format_instruction<textarea id="formatInstruction"></textarea></label>
              <label>completion_instruction<textarea id="completionInstruction"></textarea></label>
              <label>extra API parameters<textarea id="extraParams" placeholder='{"web_search_options": {}, "metadata": {"source": "ai-playground"}}'></textarea></label>
            </div>
          </details>
        </div>
      </section>

      <section class="prompt">
        <div class="group">
          <h2>Промпт</h2>
          <label><textarea id="prompt" placeholder="Введите запрос к модели"></textarea></label>
          <details class="panel">
            <summary>System prompt</summary>
            <div class="panel-body">
              <label>system_prompt<textarea id="systemPrompt" placeholder="Необязательная системная инструкция для модели"></textarea></label>
            </div>
          </details>
          <div class="send-row">
            <button id="send" type="button">Отправить</button>
            <button id="clear" class="secondary" type="button">Очистить ответ</button>
          </div>
        </div>
        <div id="metrics" class="metrics">
          <div class="metric"><span>Время</span><strong id="metricTime">—</strong></div>
          <div class="metric"><span>Токены</span><strong id="metricTokens">—</strong></div>
          <div class="metric"><span>Стоимость</span><strong id="metricCost">—</strong></div>
        </div>
        <pre id="output">Ответ появится здесь.</pre>
        <details id="debugDetails" class="debug">
          <summary>JSON отладка</summary>
          <div class="debug-grid">
            <div class="debug-item">
              <h3>Запрос к provider API</h3>
              <pre id="providerRequest">{}</pre>
            </div>
            <div class="debug-item">
              <h3>Ответ provider API</h3>
              <pre id="providerResponse">{}</pre>
            </div>
          </div>
        </details>
      </section>
    </div>
  </main>

  <script>
    const $ = (id) => document.getElementById(id);
    const status = $('status');
    const providerSelect = $('provider');
    let providers = [];
    let modelPricingById = new Map();
    let currentConstraints = new Map();
    let tokenProvider = null;

    function setStatus(text, isError = false) {
      status.textContent = text;
      status.className = isError ? 'status error' : 'status';
    }

    function numberValue(id) {
      const element = $(id);
      const value = element.value.trim();
      return value === '' ? null : Number(value);
    }

    function controlledNumberValue(id) {
      const constraint = currentConstraints.get(id);
      if (constraint && !constraint.supported) return null;
      const element = $(id);
      const value = element.value.trim();
      return value === '' ? null : Number(value);
    }

    function textValue(id) {
      const value = $(id).value;
      return value.trim() === '' ? null : value;
    }

    function boolValue(id) {
      const constraint = currentConstraints.get(id);
      if (constraint && !constraint.supported) return null;
      return $(id).checked;
    }

    function optionalBoolValue(id) {
      const constraint = currentConstraints.get(id);
      if (constraint && !constraint.supported) return null;
      return $(id).checked ? true : null;
    }

    function applyProviderConstraints(selected) {
      currentConstraints = new Map((selected.parameter_constraints || []).map((item) => [item.id, item]));
      for (const [id, constraint] of currentConstraints) {
        const element = $(id);
        if (!element) continue;
        element.disabled = !constraint.supported;
        element.closest('label')?.classList.toggle('unsupported', !constraint.supported);
        if (constraint.min === null || constraint.min === undefined) element.removeAttribute('min');
        else element.min = String(constraint.min);
        if (constraint.max === null || constraint.max === undefined) element.removeAttribute('max');
        else element.max = String(constraint.max);
        if (constraint.step === null || constraint.step === undefined) element.removeAttribute('step');
        else element.step = String(constraint.step);
      }
      validateParameterConstraints();
    }

    function validateParameterConstraints() {
      const messages = [];
      for (const [id, constraint] of currentConstraints) {
        const element = $(id);
        if (!element) continue;
        const label = element.closest('label');
        label?.classList.remove('field-warning');
        if (!constraint.supported) {
          if (hasUserEditedUnsupportedValue(element)) {
            messages.push(`${labelText(label)}: ${constraint.note}`);
            label?.classList.add('field-warning');
          }
          continue;
        }
        if (id === 'parallelToolCalls' && element.checked) {
          messages.push(`${labelText(label)}: ${constraint.note}`);
          label?.classList.add('field-warning');
        }
        if (element.type === 'number' && element.value.trim() !== '') {
          const value = Number(element.value);
          if (constraint.min !== null && constraint.min !== undefined && value < constraint.min) {
            messages.push(`${labelText(label)}: ${value} меньше минимума ${constraint.min}. ${constraint.note}`);
            label?.classList.add('field-warning');
          }
          if (constraint.max !== null && constraint.max !== undefined && value > constraint.max) {
            messages.push(`${labelText(label)}: ${value} больше максимума ${constraint.max}. ${constraint.note}`);
            label?.classList.add('field-warning');
          }
        }
      }
      const warnings = $('parameterWarnings');
      warnings.innerHTML = messages.map(escapeHtml).join('<br>');
      warnings.classList.toggle('visible', messages.length > 0);
      return messages;
    }

    function labelText(label) {
      return (label?.textContent || 'parameter').trim().split(/\s+/)[0];
    }

    function hasUserEditedUnsupportedValue(element) {
      if (element.type === 'checkbox') {
        return element.checked !== element.defaultChecked;
      }
      const value = element.value.trim();
      const defaultValue = element.defaultValue.trim();
      return value !== '' && value !== defaultValue;
    }

    function extraParamsValue() {
      const raw = $('extraParams').value.trim();
      if (!raw) return {};
      const parsed = JSON.parse(raw);
      if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
        throw new Error('extra API parameters должен быть JSON object');
      }
      return parsed;
    }

    function providerPayload() {
      return {
        provider: $('provider').value,
        base_url: $('baseUrl').value.trim(),
        token: $('token').value.trim(),
        token_provider: tokenProvider
      };
    }

    function currentProvider() {
      return $('provider').value;
    }

    function resetTokenOverrideForProvider() {
      $('token').value = '';
      tokenProvider = currentProvider();
    }

    function markTokenOverrideProvider() {
      tokenProvider = currentProvider();
    }

    function selectedModel() {
      return $('customModel').value.trim() || $('model').value.trim();
    }

    function selectedModelPricing() {
      return modelPricingById.get(selectedModel()) || null;
    }

    function chatPayload() {
      return {
        ...providerPayload(),
        model: selectedModel(),
        system_prompt: textValue('systemPrompt'),
        prompt: $('prompt').value,
        pricing: pricingPayload(),
        billing: billingPayload(),
        control: {
          response_format: $('responseFormat').value,
          answer_format: $('answerFormat').value,
          max_tokens: controlledNumberValue('maxTokens'),
          max_completion_tokens: controlledNumberValue('maxCompletionTokens'),
          temperature: controlledNumberValue('temperature'),
          top_p: controlledNumberValue('topP'),
          top_k: controlledNumberValue('topK'),
          min_p: controlledNumberValue('minP'),
          top_a: controlledNumberValue('topA'),
          presence_penalty: controlledNumberValue('presencePenalty'),
          frequency_penalty: controlledNumberValue('frequencyPenalty'),
          repetition_penalty: controlledNumberValue('repetitionPenalty'),
          seed: numberValue('seed'),
          reasoning_effort: textValue('reasoningEffort'),
          include_reasoning: boolValue('includeReasoning'),
          verbosity: textValue('verbosity'),
          logprobs: boolValue('logprobs'),
          top_logprobs: controlledNumberValue('topLogprobs'),
          n: controlledNumberValue('n'),
          store: boolValue('store'),
          parallel_tool_calls: optionalBoolValue('parallelToolCalls'),
          user: textValue('user'),
          service_tier: textValue('serviceTier'),
          extra_params: extraParamsValue(),
          stop: $('stop').value.split('\n').map((item) => item.trim()).filter(Boolean),
          answer_prefix: textValue('answerPrefix'),
          answer_suffix: textValue('answerSuffix'),
          address_as: textValue('addressAs'),
          quote_question: $('quoteQuestion').checked,
          format_instruction: textValue('formatInstruction'),
          completion_instruction: textValue('completionInstruction')
        }
      };
    }

    function pricingPayload() {
      applySelectedModelPricingIfFieldsAreEmpty();
      return {
        input_per_million: numberValue('priceInput'),
        output_per_million: numberValue('priceOutput'),
        cache_hit_input_per_million: numberValue('priceCacheHitInput'),
        cache_miss_input_per_million: numberValue('priceCacheMissInput'),
        currency: textValue('priceCurrency')
      };
    }

    function applySelectedModelPricingIfFieldsAreEmpty() {
      const pricing = selectedModelPricing();
      if (!pricing) return;
      if (!$('priceInput').value.trim()) $('priceInput').value = pricing.input_per_million ?? '';
      if (!$('priceOutput').value.trim()) $('priceOutput').value = pricing.output_per_million ?? '';
      if (!$('priceCacheHitInput').value.trim()) $('priceCacheHitInput').value = pricing.cache_hit_input_per_million ?? '';
      if (!$('priceCacheMissInput').value.trim()) $('priceCacheMissInput').value = pricing.cache_miss_input_per_million ?? '';
      if (!$('priceCurrency').value.trim()) $('priceCurrency').value = pricing.currency || 'USD';
    }

    function applySelectedModelPricing() {
      const pricing = selectedModelPricing();
      $('priceInput').value = pricing?.input_per_million ?? '';
      $('priceOutput').value = pricing?.output_per_million ?? '';
      $('priceCacheHitInput').value = pricing?.cache_hit_input_per_million ?? '';
      $('priceCacheMissInput').value = pricing?.cache_miss_input_per_million ?? '';
      $('priceCurrency').value = pricing?.currency || 'USD';
    }

    function billingPayload() {
      return {
        openai_admin_token: textValue('openaiAdminToken'),
        openai_cost_poll_seconds: numberValue('openaiCostPollSeconds')
      };
    }

    function prettyJson(value) {
      return JSON.stringify(value ?? {}, null, 2);
    }

    function setDebug(debug) {
      $('providerRequest').textContent = prettyJson(debug?.provider_request);
      $('providerResponse').textContent = prettyJson(debug?.provider_response);
    }

    function setMetrics(metrics) {
      if (!metrics) {
        $('metricTime').textContent = '—';
        $('metricTokens').textContent = '—';
        $('metricCost').textContent = '—';
        return;
      }
      $('metricTime').textContent = `${metrics.elapsed_ms} ms`;
      $('metricTokens').innerHTML = metrics.usage ? tokenLines(metrics.usage) : 'unavailable';
      $('metricCost').textContent = metrics.cost
        ? `${Number(metrics.cost.amount).toFixed(8)} ${metrics.cost.currency} (${metrics.cost.source})`
        : 'unavailable';
    }

    function tokenLines(usage) {
      const rows = [
        ['input', usage.input_tokens],
        ['output', usage.output_tokens],
        ['total', usage.total_tokens]
      ];
      if (usage.cache_hit_input_tokens !== null && usage.cache_hit_input_tokens !== undefined) {
        rows.push(['cache hit', usage.cache_hit_input_tokens]);
      }
      if (usage.cache_miss_input_tokens !== null && usage.cache_miss_input_tokens !== undefined) {
        rows.push(['cache miss', usage.cache_miss_input_tokens]);
      }
      return `<span class="metric-lines">${rows.map(([label, value]) => `<span class="metric-line"><em>${label}</em><b>${value}</b></span>`).join('')}</span>`;
    }

    async function requestJson(url, body) {
      let response;
      try {
        response = await fetch(url, {
          method: body ? 'POST' : 'GET',
          headers: body ? { 'Content-Type': 'application/json' } : {},
          body: body ? JSON.stringify(body) : undefined
        });
      } catch {
        throw new Error('Локальный сервер недоступен. Запустите `ai web` и обновите страницу.');
      }
      const raw = await response.text();
      let data = {};
      try {
        data = raw ? JSON.parse(raw) : {};
      } catch {
        data = { error: raw || 'Request failed' };
      }
      if (!response.ok) {
        throw new Error(data.error || 'Request failed');
      }
      return data;
    }

    function applyProviderDefaults() {
      const selected = providers.find((item) => item.id === providerSelect.value);
      if (!selected) return;
      $('baseUrl').value = selected.default_base_url;
      $('customModel').value = '';
      setModelOptions([{ id: selected.default_model, pricing: null }], selected.default_model);
      applyProviderConstraints(selected);
    }

    function setModelOptions(models, selectedModel) {
      const normalized = models.map((model) => typeof model === 'string' ? { id: model, pricing: null } : model).filter((model) => model?.id);
      const uniqueModels = [...new Map(normalized.map((model) => [model.id, model])).values()];
      modelPricingById = new Map(uniqueModels.filter((model) => model.pricing).map((model) => [model.id, model.pricing]));
      $('model').innerHTML = uniqueModels.map((model) => `<option value="${escapeHtml(model.id)}">${escapeHtml(model.id)}</option>`).join('');
      if (selectedModel && uniqueModels.some((model) => model.id === selectedModel)) {
        $('model').value = selectedModel;
      }
      applySelectedModelPricing();
    }

    function escapeHtml(value) {
      return value
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;')
        .replaceAll("'", '&#039;');
    }

    async function init() {
      const data = await requestJson('/api/providers');
      providers = data.providers;
      providerSelect.innerHTML = providers.map((item) => `<option value="${item.id}">${item.name}</option>`).join('');
      resetTokenOverrideForProvider();
      applyProviderDefaults();
      setStatus('Готово');
    }

    async function loadModels() {
      $('loadModels').disabled = true;
      setStatus('Загружаю модели...');
      try {
        const data = await requestJson('/api/models', providerPayload());
        const current = selectedModel();
        const modelIds = data.models.map((model) => model.id);
        const fallback = modelIds[0] || current;
        setModelOptions(data.models, modelIds.includes(current) ? current : fallback);
        $('customModel').value = '';
        setStatus(data.models.length ? `Загружено моделей: ${data.models.length}; выбрана ${$('model').value}` : 'Провайдер вернул пустой список моделей');
      } catch (error) {
        setStatus(error.message, true);
      } finally {
        $('loadModels').disabled = false;
      }
    }

    async function sendPrompt() {
      $('send').disabled = true;
      setStatus('Жду ответ модели...');
      $('output').textContent = '';
      setDebug(null);
      setMetrics(null);
      try {
        const payload = chatPayload();
        const data = await requestJson('/api/chat', payload);
        $('output').textContent = data.text;
        setMetrics(data.metrics);
        setDebug(data.debug);
        setStatus(data.finish_reason ? `Готово: ${data.finish_reason}` : 'Готово');
      } catch (error) {
        $('output').textContent = error.message;
        setStatus(error.message, true);
      } finally {
        $('send').disabled = false;
      }
    }

    providerSelect.addEventListener('change', () => {
      resetTokenOverrideForProvider();
      applyProviderDefaults();
    });
    $('token').addEventListener('input', markTokenOverrideProvider);
    $('model').addEventListener('change', applySelectedModelPricing);
    $('customModel').addEventListener('input', applySelectedModelPricing);
    document.querySelectorAll('input, select, textarea').forEach((element) => {
      element.addEventListener('input', validateParameterConstraints);
      element.addEventListener('change', validateParameterConstraints);
    });
    $('loadModels').addEventListener('click', loadModels);
    $('send').addEventListener('click', sendPrompt);
    $('clear').addEventListener('click', () => {
      $('output').textContent = 'Ответ появится здесь.';
      setDebug(null);
      setMetrics(null);
    });
    init().catch((error) => setStatus(error.message, true));
  </script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    fn web_profile(provider: ProviderKind) -> ProfileConfig {
        ProfileConfig {
            provider,
            model: provider.default_model().to_string(),
            base_url: provider.default_base_url().to_string(),
            token_ref: String::new(),
        }
    }

    #[test]
    fn web_token_uses_provider_keyring_when_form_is_empty() {
        let secrets = MemorySecretStore::default();
        let profile = web_profile(ProviderKind::OpenRouter);
        secrets
            .set_token("openrouter", "stored-token")
            .expect("set token");

        let token = resolve_web_token(&secrets, &profile, "", None).expect("resolve token");

        assert_eq!(token, "stored-token");
    }

    #[test]
    fn web_token_override_is_saved_like_cli_token() {
        let secrets = MemorySecretStore::default();
        let profile = web_profile(ProviderKind::DeepSeek);

        let token = resolve_web_token(&secrets, &profile, " fresh-token ", Some("deepseek"))
            .expect("resolve token");

        assert_eq!(token, "fresh-token");
        assert_eq!(
            secrets.get_token("deepseek").expect("get token"),
            Some("fresh-token".to_string())
        );
    }

    #[test]
    fn web_token_override_from_another_provider_is_ignored() {
        let secrets = MemorySecretStore::default();
        let profile = web_profile(ProviderKind::DeepSeek);
        secrets
            .set_token("deepseek", "deepseek-token")
            .expect("set deepseek token");

        let token = resolve_web_token(&secrets, &profile, " kimi-token ", Some("kimi"))
            .expect("resolve token");

        assert_eq!(token, "deepseek-token");
        assert_eq!(
            secrets.get_token("deepseek").expect("get token"),
            Some("deepseek-token".to_string())
        );
    }

    #[test]
    fn web_chat_messages_put_system_prompt_before_user_prompt() {
        let request = ChatWebRequest {
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            system_prompt: Some("Ты отвечаешь кратко.".to_string()),
            prompt: "Привет".to_string(),
            control: WebResponseControl::default(),
            pricing: None,
            billing: None,
        };

        let messages = request.chat_messages();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "Ты отвечаешь кратко.");
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content, "Привет");
    }
}
