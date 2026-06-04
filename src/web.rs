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
        AnswerFormat, ChatMessage, ChatRequest, HttpDebugRequest, HttpDebugResponse,
        ProviderClient, ProviderKind, ReqwestProviderClient, ResponseControl, ResponseFormat, Role,
        validate_base_url,
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
    let token = resolve_web_token(state.secrets.as_ref(), &profile, &request.token)?;
    let mut models = state.client.list_models(&profile, &token).await?;
    models.sort();
    Ok(Json(ModelsResponse { models }))
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
    let token = resolve_web_token(state.secrets.as_ref(), &profile, &request.token)?;
    let control = request.control.clone().into_control();
    let chat_request = ChatRequest {
        model: profile.model.clone(),
        messages: request.chat_messages(),
        control,
    };
    let backend_request = request.debug_value(&chat_request);
    let (response, provider_debug) = state
        .client
        .chat_completion_with_debug(&profile, &token, chat_request)
        .await?;
    let backend_response = serde_json::json!({
        "text": response.text,
        "finish_reason": response.finish_reason,
    });
    Ok(Json(ChatWebResponse {
        text: backend_response["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        finish_reason: backend_response["finish_reason"]
            .as_str()
            .map(ToString::to_string),
        debug: ChatDebugView {
            backend_request,
            provider_request: provider_debug.request,
            provider_response: provider_debug.response,
            backend_response,
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
    models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChatWebRequest {
    provider: String,
    base_url: String,
    token: String,
    model: String,
    system_prompt: Option<String>,
    prompt: String,
    control: WebResponseControl,
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

    fn debug_value(&self, chat_request: &ChatRequest) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider,
            "base_url": self.base_url,
            "token": redacted_token_value(&self.token),
            "model": self.model,
            "system_prompt": self.system_prompt,
            "prompt": self.prompt,
            "provider_chat_request": chat_request,
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
    debug: ChatDebugView,
}

#[derive(Debug, Serialize)]
struct ChatDebugView {
    backend_request: serde_json::Value,
    provider_request: HttpDebugRequest,
    provider_response: HttpDebugResponse,
    backend_response: serde_json::Value,
}

fn resolve_web_token(
    secrets: &dyn SecretStore,
    profile: &ProfileConfig,
    token_override: &str,
) -> Result<String, AppError> {
    let token_override = token_override.trim();
    if !token_override.is_empty() {
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

fn parse_provider(value: &str) -> Result<ProviderKind, AppError> {
    value.parse()
}

fn redacted_token_value(token: &str) -> serde_json::Value {
    if token.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String("[redacted]".to_string())
    }
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
      grid-template-columns: minmax(320px, 420px) minmax(0, 1fr);
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
      min-height: 156px;
      resize: vertical;
      line-height: 1.45;
    }
    .prompt textarea {
      min-height: 260px;
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
      min-height: 420px;
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
          <label>API token override<input id="token" type="password" autocomplete="off" spellcheck="false" placeholder="Пусто = взять из keychain"></label>
          <label>Base URL<input id="baseUrl" spellcheck="false"></label>
          <label>Model<select id="model"></select></label>
          <label>Custom model id<input id="customModel" spellcheck="false" placeholder="Если нужной модели нет в списке"></label>
          <div class="actions">
            <button id="loadModels" class="secondary" type="button">Загрузить модели</button>
          </div>
        </div>

        <div class="group">
          <h2>API параметры</h2>
          <div id="parameterWarnings" class="warnings"></div>
          <div class="row">
            <label>response_format<select id="responseFormat"><option value="text">text</option><option value="json-object">json_object</option></select></label>
            <label>answer_format<select id="answerFormat"><option value="natural">natural</option><option value="bullets">bullets</option><option value="numbered">numbered</option><option value="short">short</option><option value="steps">steps</option><option value="table">table</option></select></label>
          </div>
          <div class="row">
            <label>max_tokens<input id="maxTokens" type="number" min="1" step="1" value="1024"></label>
            <label>max_completion_tokens<input id="maxCompletionTokens" type="number" min="1" step="1"></label>
          </div>
          <div class="row">
            <label>temperature<input id="temperature" type="number" min="0" max="2" step="0.1" value="1"></label>
            <label>top_p<input id="topP" type="number" min="0" max="1" step="0.05" value="1"></label>
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
        </div>

        <div class="group">
          <h2>Reasoning и вывод</h2>
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

        <div class="group">
          <h2>Формат ответа</h2>
          <div class="row">
            <label>answer_prefix<input id="answerPrefix"></label>
            <label>answer_suffix<input id="answerSuffix"></label>
          </div>
          <label>address_as<input id="addressAs"></label>
          <label class="inline"><input id="quoteQuestion" type="checkbox"> quote_question</label>
          <label>format_instruction<textarea id="formatInstruction"></textarea></label>
          <label>completion_instruction<textarea id="completionInstruction"></textarea></label>
        </div>

        <div class="group">
          <h2>Дополнительный JSON</h2>
          <label>extra API parameters<textarea id="extraParams" placeholder='{"web_search_options": {}, "metadata": {"source": "ai-playground"}}'></textarea></label>
        </div>
      </section>

      <section class="prompt">
        <div class="group">
          <h2>Промпт</h2>
          <label>system_prompt<textarea id="systemPrompt" placeholder="Необязательная системная инструкция для модели"></textarea></label>
          <label><textarea id="prompt" placeholder="Введите запрос к модели"></textarea></label>
          <div class="actions">
            <button id="send" type="button">Отправить</button>
            <button id="clear" class="secondary" type="button">Очистить ответ</button>
          </div>
        </div>
        <pre id="output">Ответ появится здесь.</pre>
        <details id="debugDetails" class="debug">
          <summary>JSON отладка</summary>
          <div class="debug-grid">
            <div class="debug-item">
              <h3>Запрос в backend</h3>
              <pre id="backendRequest">{}</pre>
            </div>
            <div class="debug-item">
              <h3>Запрос к provider API</h3>
              <pre id="providerRequest">{}</pre>
            </div>
            <div class="debug-item">
              <h3>Ответ provider API</h3>
              <pre id="providerResponse">{}</pre>
            </div>
            <div class="debug-item">
              <h3>Ответ backend</h3>
              <pre id="backendResponse">{}</pre>
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
    let currentConstraints = new Map();

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
        token: $('token').value.trim()
      };
    }

    function selectedModel() {
      return $('customModel').value.trim() || $('model').value.trim();
    }

    function chatPayload() {
      return {
        ...providerPayload(),
        model: selectedModel(),
        system_prompt: textValue('systemPrompt'),
        prompt: $('prompt').value,
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

    function prettyJson(value) {
      return JSON.stringify(value ?? {}, null, 2);
    }

    function setDebug(debug) {
      $('backendRequest').textContent = prettyJson(debug?.backend_request);
      $('providerRequest').textContent = prettyJson(debug?.provider_request);
      $('providerResponse').textContent = prettyJson(debug?.provider_response);
      $('backendResponse').textContent = prettyJson(debug?.backend_response);
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
      setModelOptions([selected.default_model], selected.default_model);
      applyProviderConstraints(selected);
    }

    function setModelOptions(models, selectedModel) {
      const uniqueModels = [...new Set(models.filter(Boolean))];
      $('model').innerHTML = uniqueModels.map((model) => `<option value="${escapeHtml(model)}">${escapeHtml(model)}</option>`).join('');
      if (selectedModel && uniqueModels.includes(selectedModel)) {
        $('model').value = selectedModel;
      }
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
      applyProviderDefaults();
      setStatus('Готово');
    }

    async function loadModels() {
      $('loadModels').disabled = true;
      setStatus('Загружаю модели...');
      try {
        const data = await requestJson('/api/models', providerPayload());
        const current = selectedModel();
        const fallback = data.models[0] || current;
        setModelOptions(data.models, data.models.includes(current) ? current : fallback);
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
      try {
        const payload = chatPayload();
        const data = await requestJson('/api/chat', payload);
        $('output').textContent = data.text;
        setDebug(data.debug);
        setStatus(data.finish_reason ? `Готово: ${data.finish_reason}` : 'Готово');
      } catch (error) {
        $('output').textContent = error.message;
        setStatus(error.message, true);
      } finally {
        $('send').disabled = false;
      }
    }

    providerSelect.addEventListener('change', applyProviderDefaults);
    document.querySelectorAll('input, select, textarea').forEach((element) => {
      element.addEventListener('input', validateParameterConstraints);
      element.addEventListener('change', validateParameterConstraints);
    });
    $('loadModels').addEventListener('click', loadModels);
    $('send').addEventListener('click', sendPrompt);
    $('clear').addEventListener('click', () => {
      $('output').textContent = 'Ответ появится здесь.';
      setDebug(null);
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

        let token = resolve_web_token(&secrets, &profile, "").expect("resolve token");

        assert_eq!(token, "stored-token");
    }

    #[test]
    fn web_token_override_is_saved_like_cli_token() {
        let secrets = MemorySecretStore::default();
        let profile = web_profile(ProviderKind::DeepSeek);

        let token = resolve_web_token(&secrets, &profile, " fresh-token ").expect("resolve token");

        assert_eq!(token, "fresh-token");
        assert_eq!(
            secrets.get_token("deepseek").expect("get token"),
            Some("fresh-token".to_string())
        );
    }

    #[test]
    fn web_chat_messages_put_system_prompt_before_user_prompt() {
        let request = ChatWebRequest {
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            model: "deepseek-chat".to_string(),
            system_prompt: Some("Ты отвечаешь кратко.".to_string()),
            prompt: "Привет".to_string(),
            control: WebResponseControl::default(),
        };

        let messages = request.chat_messages();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "Ты отвечаешь кратко.");
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content, "Привет");
    }
}
