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
    chat::{ChatAgent, LocalSessionStore, web_session_key},
    config::{AppConfig, ProfileConfig, token_ref},
    errors::AppError,
    providers::{
        AnswerFormat, BillingLookup, BillingProvider, ChatMessage, HttpDebugRequest,
        HttpDebugResponse, ModelPricing, ProviderKind, ReqwestProviderClient, ResponseControl,
        ResponseFormat, Role, validate_base_url,
    },
    secrets::{KeyringSecretStore, SecretStore, get_config_profile_token, set_profile_token},
};

const INDEX_HTML: &str = include_str!("ui.html");

#[derive(Clone)]
struct AppState {
    client: ReqwestProviderClient,
    secrets: Arc<dyn SecretStore>,
    sessions: LocalSessionStore,
}

pub async fn serve(addr: SocketAddr) -> Result<(), AppError> {
    let client = ReqwestProviderClient::new()?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sessions = LocalSessionStore::new()?;
    let app = Router::new()
        .route("/", get(index))
        .route("/api/providers", get(providers))
        .route("/api/models", post(models))
        .route("/api/chat/session", post(chat_session))
        .route("/api/chat", post(chat))
        .with_state(AppState {
            client,
            secrets,
            sessions,
        });
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
    let session_key = web_session_key(&profile.provider.to_string(), &profile.model);
    let session = if request.new_session {
        state.sessions.create_session()?
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    let control = request.control.clone().into_control();
    let mut agent = ChatAgent::new(
        profile.clone(),
        token,
        request.initial_history(session.messages),
        control,
        request
            .pricing
            .clone()
            .and_then(WebPricing::into_model_pricing),
        request
            .billing
            .clone()
            .and_then(WebBilling::into_billing_lookup),
    );
    let (response, provider_debug) = agent
        .respond_with_debug(&state.client, request.prompt.clone())
        .await?;
    state
        .sessions
        .save_session(&session_key, &session.id, agent.history())?;
    Ok(Json(ChatWebResponse {
        session_id: session.id,
        text: response.text,
        finish_reason: response.finish_reason,
        metrics: response.metrics,
        messages: agent.history().to_vec(),
        debug: ChatDebugView {
            provider_request: provider_debug.request,
            provider_response: provider_debug.response,
        },
    }))
}

async fn chat_session(
    State(state): State<AppState>,
    Json(request): Json<ChatSessionRequest>,
) -> Result<Json<ChatSessionResponse>, WebError> {
    let provider = parse_provider(&request.provider)?;
    let model = blank_to_none(Some(request.model))
        .ok_or_else(|| AppError::InvalidInput("Model is required".to_string()))?;
    let session_key = web_session_key(&provider.to_string(), &model);
    let session = if request.new_session {
        let session = state.sessions.create_session()?;
        state
            .sessions
            .save_session(&session_key, &session.id, &session.messages)?;
        session
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    Ok(Json(ChatSessionResponse {
        session_id: session.id,
        messages: session.messages,
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
    session_id: Option<String>,
    new_session: bool,
    messages: Option<Vec<ChatMessage>>,
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

    fn initial_history(&self, stored_messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        let mut messages = if stored_messages.is_empty() {
            self.messages.clone().unwrap_or_default()
        } else {
            stored_messages
        };
        if let Some(system_prompt) = blank_to_none(self.system_prompt.clone()) {
            let has_system = messages.iter().any(|message| message.role == Role::System);
            if !has_system {
                messages.insert(
                    0,
                    ChatMessage {
                        role: Role::System,
                        content: system_prompt,
                    },
                );
            }
        }
        messages
    }
}

#[derive(Debug, Deserialize)]
struct ChatSessionRequest {
    provider: String,
    model: String,
    session_id: Option<String>,
    new_session: bool,
}

#[derive(Debug, Serialize)]
struct ChatSessionResponse {
    session_id: String,
    messages: Vec<ChatMessage>,
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
            input_per_million: self.input_per_million,
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
    session_id: String,
    text: String,
    finish_reason: Option<String>,
    metrics: crate::providers::RequestMetrics,
    messages: Vec<ChatMessage>,
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

fn blank_str_to_none(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
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
            session_id: None,
            new_session: false,
            messages: None,
            control: WebResponseControl::default(),
            pricing: None,
            billing: None,
        };

        let messages = request.initial_history(Vec::new());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "Ты отвечаешь кратко.");
    }

    #[test]
    fn web_chat_history_keeps_prior_messages_and_does_not_duplicate_system_prompt() {
        let request = ChatWebRequest {
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            system_prompt: Some("Ты отвечаешь кратко.".to_string()),
            prompt: "Продолжи".to_string(),
            session_id: None,
            new_session: false,
            messages: Some(vec![
                ChatMessage {
                    role: Role::System,
                    content: "Ты отвечаешь кратко.".to_string(),
                },
                ChatMessage {
                    role: Role::User,
                    content: "Привет".to_string(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: "Здравствуйте.".to_string(),
                },
            ]),
            control: WebResponseControl::default(),
            pricing: None,
            billing: None,
        };

        let messages = request.initial_history(Vec::new());

        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.role == Role::System)
                .count(),
            1
        );
    }

    #[test]
    fn web_chat_initial_history_prefers_local_store_over_client_messages() {
        let request = ChatWebRequest {
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            system_prompt: None,
            prompt: "Продолжи".to_string(),
            session_id: Some("session".to_string()),
            new_session: false,
            messages: Some(vec![ChatMessage {
                role: Role::User,
                content: "client".to_string(),
            }]),
            control: WebResponseControl::default(),
            pricing: None,
            billing: None,
        };

        let messages = request.initial_history(vec![ChatMessage {
            role: Role::Assistant,
            content: "stored".to_string(),
        }]);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "stored");
    }

    /// Баг 2: WebPricing с только output ценой должен конвертироваться в Some(ModelPricing)
    #[test]
    fn web_pricing_output_only_creates_model_pricing() {
        let pricing = WebPricing {
            input_per_million: None,
            output_per_million: Some(4.0),
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
            currency: Some("USD".to_string()),
        };

        let result = pricing.into_model_pricing();

        let mp = result.expect("должен вернуть Some даже без input цены");
        assert!(mp.input_per_million.is_none());
        assert!((mp.output_per_million - 4.0).abs() < f64::EPSILON);
        assert_eq!(mp.currency, "USD");
    }

    /// Без output цены — ModelPricing не имеет смысла, возвращаем None
    #[test]
    fn web_pricing_without_output_returns_none() {
        let pricing = WebPricing {
            input_per_million: Some(2.0),
            output_per_million: None,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
            currency: None,
        };

        assert!(pricing.into_model_pricing().is_none());
    }

    /// Полные цены — оба поля присутствуют
    #[test]
    fn web_pricing_with_both_prices_creates_correct_model_pricing() {
        let pricing = WebPricing {
            input_per_million: Some(1.5),
            output_per_million: Some(6.0),
            cache_hit_input_per_million: Some(0.1),
            cache_miss_input_per_million: None,
            currency: Some("USD".to_string()),
        };

        let mp = pricing.into_model_pricing().expect("Some");

        assert_eq!(mp.input_per_million, Some(1.5));
        assert_eq!(mp.output_per_million, 6.0);
        assert_eq!(mp.cache_hit_input_per_million, Some(0.1));
        assert!(mp.cache_miss_input_per_million.is_none());
    }

    /// Пустая валюта → дефолт USD
    #[test]
    fn web_pricing_blank_currency_defaults_to_usd() {
        let pricing = WebPricing {
            input_per_million: None,
            output_per_million: Some(1.0),
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
            currency: Some("   ".to_string()),
        };

        let mp = pricing.into_model_pricing().expect("Some");
        assert_eq!(mp.currency, "USD");
    }
}
