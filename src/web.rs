use std::net::SocketAddr;

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
    config::ProfileConfig,
    errors::AppError,
    providers::{
        AnswerFormat, ChatMessage, ChatRequest, ProviderClient, ProviderKind,
        ReqwestProviderClient, ResponseControl, ResponseFormat, Role, validate_base_url,
    },
};

#[derive(Clone)]
struct AppState {
    client: ReqwestProviderClient,
}

pub async fn serve(addr: SocketAddr) -> Result<(), AppError> {
    let client = ReqwestProviderClient::new()?;
    let app = Router::new()
        .route("/", get(index))
        .route("/api/providers", get(providers))
        .route("/api/models", post(models))
        .route("/api/chat", post(chat))
        .with_state(AppState { client });
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
    let mut models = state.client.list_models(&profile, &request.token).await?;
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
    let response = state
        .client
        .chat_completion(
            &profile,
            &request.token,
            ChatRequest {
                model: profile.model.clone(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: request.prompt,
                }],
                control: request.control.into_control(),
            },
        )
        .await?;
    Ok(Json(ChatWebResponse {
        text: response.text,
        finish_reason: response.finish_reason,
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
}

#[derive(Debug, Deserialize)]
struct ModelsRequest {
    provider: String,
    base_url: String,
    token: String,
}

impl ModelsRequest {
    fn profile(&self) -> Result<ProfileConfig, AppError> {
        ensure_token(&self.token)?;
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
    prompt: String,
    control: WebResponseControl,
}

impl ChatWebRequest {
    fn profile(&self) -> Result<ProfileConfig, AppError> {
        ensure_token(&self.token)?;
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
}

#[derive(Debug, Deserialize, Default)]
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
}

fn ensure_token(token: &str) -> Result<(), AppError> {
    if token.trim().is_empty() {
        Err(AppError::InvalidInput("API token is required".to_string()))
    } else {
        Ok(())
    }
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
  <title>aiteach web</title>
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
      <h1>aiteach</h1>
      <div id="status" class="status">Готово</div>
    </header>

    <div class="layout">
      <section class="controls">
        <div class="group">
          <h2>Провайдер</h2>
          <label>Provider<select id="provider"></select></label>
          <label>API token<input id="token" type="password" autocomplete="off" spellcheck="false"></label>
          <label>Base URL<input id="baseUrl" spellcheck="false"></label>
          <label>Model<select id="model"></select></label>
          <label>Custom model id<input id="customModel" spellcheck="false" placeholder="Если нужной модели нет в списке"></label>
          <div class="actions">
            <button id="loadModels" class="secondary" type="button">Загрузить модели</button>
          </div>
        </div>

        <div class="group">
          <h2>API параметры</h2>
          <div class="row">
            <label>response_format<select id="responseFormat"><option value="text">text</option><option value="json-object">json_object</option></select></label>
            <label>answer_format<select id="answerFormat"><option value="natural">natural</option><option value="bullets">bullets</option><option value="numbered">numbered</option><option value="short">short</option><option value="steps">steps</option><option value="table">table</option></select></label>
          </div>
          <div class="row">
            <label>max_tokens<input id="maxTokens" type="number" min="1" step="1" value="1024" data-default="1024"></label>
            <label>max_completion_tokens<input id="maxCompletionTokens" type="number" min="1" step="1"></label>
          </div>
          <div class="row">
            <label>temperature<input id="temperature" type="number" min="0" max="2" step="0.1" value="1" data-default="1"></label>
            <label>top_p<input id="topP" type="number" min="0" max="1" step="0.05" value="1" data-default="1"></label>
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
            <label>presence_penalty<input id="presencePenalty" type="number" min="-2" max="2" step="0.1" value="0" data-default="0"></label>
            <label>frequency_penalty<input id="frequencyPenalty" type="number" min="-2" max="2" step="0.1" value="0" data-default="0"></label>
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
            <label>n<input id="n" type="number" min="1" step="1" value="1" data-default="1"></label>
            <label>top_logprobs<input id="topLogprobs" type="number" min="0" max="20" step="1"></label>
          </div>
          <div class="row">
            <label>service_tier<select id="serviceTier"><option value="">provider default</option><option value="auto">auto</option><option value="default">default</option><option value="flex">flex</option><option value="priority">priority</option></select></label>
            <label>user<input id="user" spellcheck="false"></label>
          </div>
          <label class="inline"><input id="includeReasoning" type="checkbox" data-default="false"> include_reasoning</label>
          <label class="inline"><input id="logprobs" type="checkbox" data-default="false"> logprobs</label>
          <label class="inline"><input id="store" type="checkbox" data-default="false"> store</label>
          <label class="inline"><input id="parallelToolCalls" type="checkbox" checked data-default="true"> parallel_tool_calls</label>
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
          <label>extra API parameters<textarea id="extraParams" placeholder='{"web_search_options": {}, "metadata": {"source": "aiteach"}}'></textarea></label>
        </div>
      </section>

      <section class="prompt">
        <div class="group">
          <h2>Промпт</h2>
          <label><textarea id="prompt" placeholder="Введите запрос к модели"></textarea></label>
          <div class="actions">
            <button id="send" type="button">Отправить</button>
            <button id="clear" class="secondary" type="button">Очистить ответ</button>
          </div>
        </div>
        <pre id="output">Ответ появится здесь.</pre>
      </section>
    </div>
  </main>

  <script>
    const $ = (id) => document.getElementById(id);
    const status = $('status');
    const providerSelect = $('provider');
    let providers = [];

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
      const element = $(id);
      const value = element.value.trim();
      if (value === '') return null;
      if (element.dataset.default !== undefined && value === element.dataset.default) return null;
      return Number(value);
    }

    function textValue(id) {
      const value = $(id).value;
      return value.trim() === '' ? null : value;
    }

    function boolValue(id) {
      const element = $(id);
      const checked = element.checked;
      if (element.dataset.default !== undefined && String(checked) === element.dataset.default) {
        return null;
      }
      return checked;
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
        prompt: $('prompt').value,
        control: {
          response_format: $('responseFormat').value,
          answer_format: $('answerFormat').value,
          max_tokens: controlledNumberValue('maxTokens'),
          max_completion_tokens: numberValue('maxCompletionTokens'),
          temperature: controlledNumberValue('temperature'),
          top_p: controlledNumberValue('topP'),
          top_k: numberValue('topK'),
          min_p: numberValue('minP'),
          top_a: numberValue('topA'),
          presence_penalty: controlledNumberValue('presencePenalty'),
          frequency_penalty: controlledNumberValue('frequencyPenalty'),
          repetition_penalty: numberValue('repetitionPenalty'),
          seed: numberValue('seed'),
          reasoning_effort: textValue('reasoningEffort'),
          include_reasoning: boolValue('includeReasoning'),
          verbosity: textValue('verbosity'),
          logprobs: boolValue('logprobs'),
          top_logprobs: numberValue('topLogprobs'),
          n: controlledNumberValue('n'),
          store: boolValue('store'),
          parallel_tool_calls: boolValue('parallelToolCalls'),
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

    async function requestJson(url, body) {
      let response;
      try {
        response = await fetch(url, {
          method: body ? 'POST' : 'GET',
          headers: body ? { 'Content-Type': 'application/json' } : {},
          body: body ? JSON.stringify(body) : undefined
        });
      } catch {
        throw new Error('Локальный сервер недоступен. Запустите `aiteach web` и обновите страницу.');
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
      try {
        const payload = chatPayload();
        const data = await requestJson('/api/chat', payload);
        $('output').textContent = data.text;
        setStatus(data.finish_reason ? `Готово: ${data.finish_reason}` : 'Готово');
      } catch (error) {
        $('output').textContent = error.message;
        setStatus(error.message, true);
      } finally {
        $('send').disabled = false;
      }
    }

    providerSelect.addEventListener('change', applyProviderDefaults);
    $('loadModels').addEventListener('click', loadModels);
    $('send').addEventListener('click', sendPrompt);
    $('clear').addEventListener('click', () => { $('output').textContent = 'Ответ появится здесь.'; });
    init().catch((error) => setStatus(error.message, true));
  </script>
</body>
</html>
"#;
