use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    response::{
        Html,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

use crate::{
    chat::memory::{MemoryConfig, MemoryStrategy},
    chat::{
        AgentMemory, ChatAgent, LocalSessionStore, add_request_metrics, available_agents,
        selected_agent, web_session_key,
    },
    config::ProfileConfig,
    errors::AppError,
    pricing::{LiteLlmPriceCatalog, PriceCatalogStatus, PricingResolution},
    providers::{
        AnswerFormat, BillingLookup, BillingProvider, ChatMessage, HttpDebugRequest,
        HttpDebugResponse, ModelPricing, ProviderKind, ReqwestProviderClient, ResponseControl,
        ResponseFormat, Role, validate_base_url,
    },
    secrets::{KeyringSecretStore, SecretStore, set_profile_token},
};

mod error;
mod parameters;
mod tokens;
mod util;

use error::WebError;
use parameters::{ParameterConstraintView, parameter_constraints};
use tokens::{resolve_web_token, token_override_belongs_to_provider, web_token_present};
use util::{blank_str_to_none, blank_to_none, parse_provider};

const INDEX_HTML: &str = include_str!("ui.html");

#[derive(Clone)]
struct AppState {
    client: ReqwestProviderClient,
    secrets: Arc<dyn SecretStore>,
    sessions: LocalSessionStore,
    prices: LiteLlmPriceCatalog,
}

pub async fn serve(addr: SocketAddr) -> Result<(), AppError> {
    let client = ReqwestProviderClient::new()?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sessions = LocalSessionStore::new()?;
    let prices = LiteLlmPriceCatalog::new()?;
    spawn_price_sync_task(client.clone(), prices.clone());
    let app = Router::new()
        .route("/", get(index))
        .route("/api/agents", get(agents))
        .route("/api/providers", get(providers))
        .route("/api/token/status", post(token_status))
        .route("/api/token/save", post(token_save))
        .route("/api/models", post(models))
        .route("/api/pricing/status", get(pricing_status))
        .route("/api/pricing/sync", post(pricing_sync))
        .route("/api/pricing/resolve", post(pricing_resolve))
        .route("/api/agent/session", post(chat_session))
        .route("/api/agent/chat", post(chat))
        .route("/api/agent/chat/stream", post(chat_stream))
        .route("/api/chat/session", post(chat_session))
        .route("/api/chat", post(chat))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)) // 50 MB — для вложений
        .with_state(AppState {
            client,
            secrets,
            sessions,
            prices,
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

fn spawn_price_sync_task(client: ReqwestProviderClient, prices: LiteLlmPriceCatalog) {
    tokio::spawn(async move {
        let _ = prices.sync_if_stale(client.http_client()).await;
        loop {
            sleep(Duration::from_secs(24 * 60 * 60)).await;
            let _ = prices.sync(client.http_client()).await;
        }
    });
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn agents() -> Json<AgentsResponse> {
    Json(AgentsResponse {
        agents: available_agents()
            .iter()
            .map(|agent| AgentView {
                id: agent.id.to_string(),
                name: agent.name.to_string(),
                history_storage: agent.history_storage.to_string(),
            })
            .collect(),
    })
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
    let _ = state.prices.sync_if_stale(state.client.http_client()).await;
    let mut models = state.client.list_model_info(&profile, &token).await?;
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(Json(ModelsResponse {
        models: models
            .into_iter()
            .map(|model| ModelView::from_model_info(model, &state.prices, profile.provider))
            .collect(),
    }))
}

async fn pricing_status(
    State(state): State<AppState>,
) -> Result<Json<PriceCatalogStatus>, WebError> {
    Ok(Json(state.prices.status()?))
}

async fn pricing_sync(State(state): State<AppState>) -> Result<Json<PriceCatalogStatus>, WebError> {
    Ok(Json(state.prices.sync(state.client.http_client()).await?))
}

async fn pricing_resolve(
    State(state): State<AppState>,
    Json(request): Json<PricingResolveRequest>,
) -> Result<Json<PricingResolveResponse>, WebError> {
    let provider = parse_provider(&request.provider)?;
    let _ = state.prices.sync_if_stale(state.client.http_client()).await;
    Ok(Json(PricingResolveResponse {
        pricing: state
            .prices
            .resolve(provider, &request.model)
            .ok()
            .flatten(),
    }))
}

async fn token_status(
    State(state): State<AppState>,
    Json(request): Json<ModelsRequest>,
) -> Result<Json<TokenStatusResponse>, WebError> {
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    Ok(Json(TokenStatusResponse {
        saved: web_token_present(
            state.secrets.as_ref(),
            &profile,
            &request.token,
            request.token_provider.as_deref(),
        )?,
    }))
}

async fn token_save(
    State(state): State<AppState>,
    Json(request): Json<ModelsRequest>,
) -> Result<Json<TokenStatusResponse>, WebError> {
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    if request.token.trim().is_empty() {
        return Err(AppError::InvalidInput("API token is empty".to_string()).into());
    }
    if !token_override_belongs_to_provider(&profile, request.token_provider.as_deref())? {
        return Err(AppError::InvalidInput(
            "Token override belongs to another provider".to_string(),
        )
        .into());
    }
    set_profile_token(state.secrets.as_ref(), &profile, request.token.trim())?;
    Ok(Json(TokenStatusResponse { saved: true }))
}

async fn chat(
    State(state): State<AppState>,
    Json(request): Json<ChatWebRequest>,
) -> Result<Json<ChatWebResponse>, WebError> {
    let agent_spec = selected_agent(request.agent_id.as_deref())?;
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    if request.prompt.trim().is_empty() {
        return Err(AppError::InvalidInput("Prompt is required".to_string()).into());
    }
    let prompt = build_web_prompt(&request.prompt, request.attachments.as_deref());
    let token = resolve_web_token(
        state.secrets.as_ref(),
        &profile,
        &request.token,
        request.token_provider.as_deref(),
    )?;
    let session_key = web_session_key(agent_spec.id, &profile.provider.to_string(), &profile.model);
    let session = if request.new_session {
        state.sessions.create_session()?
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    let memory = state.sessions.load_memory(&session.id)?;
    let control = request.control.clone().into_control();
    let memory_config = request
        .memory
        .clone()
        .unwrap_or_default()
        .into_memory_config();
    let _ = state.prices.sync_if_stale(state.client.http_client()).await;
    let pricing = web_request_pricing(&request, &state.prices, &profile);
    let context_limit = web_request_context_limit(&request, &state.prices, &profile);
    let mut agent = ChatAgent::new(
        profile.clone(),
        token,
        request.initial_history(session.messages),
        memory,
        control,
        pricing,
        request
            .billing
            .clone()
            .and_then(WebBilling::into_billing_lookup),
    );
    agent.set_memory_config(memory_config);
    agent.set_context_limit(context_limit);
    let (response, provider_debug, summary_metrics) = agent
        .respond_with_debug_and_summary_metrics(&state.client, prompt)
        .await?;
    let session_metrics = add_request_metrics(&session.metrics, &response.metrics);
    state
        .sessions
        .save_session(&session_key, &session.id, agent.history())?;
    state.sessions.save_metrics(&session.id, &session_metrics)?;
    state.sessions.save_memory(&session.id, agent.memory())?;
    Ok(Json(ChatWebResponse {
        agent_id: agent_spec.id.to_string(),
        session_id: session.id,
        text: response.text,
        finish_reason: response.finish_reason,
        metrics: response.metrics,
        summary_metrics,
        session_metrics,
        messages: agent.history().to_vec(),
        debug: ChatDebugView {
            provider_request: provider_debug.request,
            provider_response: provider_debug.response,
        },
    }))
}

async fn chat_stream(
    State(state): State<AppState>,
    Json(request): Json<ChatWebRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, WebError> {
    let agent_spec = selected_agent(request.agent_id.as_deref())?;
    let profile = request.profile()?;
    validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
    if request.prompt.trim().is_empty() {
        return Err(AppError::InvalidInput("Prompt is required".to_string()).into());
    }
    let prompt = build_web_prompt(&request.prompt, request.attachments.as_deref());
    let token = resolve_web_token(
        state.secrets.as_ref(),
        &profile,
        &request.token,
        request.token_provider.as_deref(),
    )?;
    let session_key = web_session_key(agent_spec.id, &profile.provider.to_string(), &profile.model);
    let session = if request.new_session {
        state.sessions.create_session()?
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    let memory = state.sessions.load_memory(&session.id)?;
    let control = request.control.clone().into_control();
    let memory_config = request
        .memory
        .clone()
        .unwrap_or_default()
        .into_memory_config();
    let _ = state.prices.sync_if_stale(state.client.http_client()).await;
    let pricing = web_request_pricing(&request, &state.prices, &profile);
    let context_limit = web_request_context_limit(&request, &state.prices, &profile);
    let mut agent = ChatAgent::new(
        profile.clone(),
        token.clone(),
        request.initial_history(session.messages),
        memory,
        control,
        pricing,
        request
            .billing
            .clone()
            .and_then(WebBilling::into_billing_lookup),
    );
    agent.set_memory_config(memory_config);
    agent.set_context_limit(context_limit);
    let (chat_request, preflight_summary_metrics) =
        agent.prepare_stream_request(&state.client, &prompt).await;
    let session_id = session.id.clone();
    let session_metrics_before = session.metrics.clone();

    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let client = state.client.clone();
    let sessions = state.sessions.clone();

    tokio::spawn(async move {
        let tx_token = tx.clone();
        let result = client
            .stream_chat_completion_with_debug(&profile, &token, chat_request, move |chunk| {
                let _ = tx_token.send(chunk.to_string());
            })
            .await;
        match result {
            Ok((response, provider_debug)) => {
                let assistant_text = response.text.clone();
                let mut response_metrics = response.metrics.clone();
                let mut summary_metrics = preflight_summary_metrics.clone();
                if let Some(summary_metrics) = preflight_summary_metrics.clone() {
                    response_metrics = add_request_metrics(&response_metrics, &summary_metrics);
                }
                let mut session_metrics =
                    add_request_metrics(&session_metrics_before, &response_metrics);
                agent.record_stream_response(prompt, assistant_text);
                if let Some(post_summary_metrics) = agent.compact_memory(&client).await {
                    summary_metrics = Some(match summary_metrics {
                        Some(current) => add_request_metrics(&current, &post_summary_metrics),
                        None => post_summary_metrics.clone(),
                    });
                    session_metrics = add_request_metrics(&session_metrics, &post_summary_metrics);
                }
                let _ = sessions.save_session(&session_key, &session_id, agent.history());
                let _ = sessions.save_metrics(&session_id, &session_metrics);
                let _ = sessions.save_memory(&session_id, agent.memory());
                let done_event = serde_json::json!({
                    "done": true,
                    "session_id": session_id,
                    "metrics": response_metrics,
                    "summary_metrics": summary_metrics,
                    "session_metrics": session_metrics,
                    "messages": agent.history(),
                    "debug": ChatDebugView {
                        provider_request: provider_debug.request,
                        provider_response: provider_debug.response,
                    },
                });
                let _ = tx.send(format!("\x00DONE\x00{done_event}"));
            }
            Err(err) => {
                let _ = tx.send(format!("\x00ERR\x00{err}"));
            }
        }
    });

    let sse_stream = stream::unfold(rx, |mut rx| async move {
        let msg = rx.recv().await?;
        if let Some(payload) = msg.strip_prefix("\x00DONE\x00") {
            let event = Event::default().event("done").data(payload.to_string());
            Some((Ok(event), rx))
        } else if let Some(payload) = msg.strip_prefix("\x00ERR\x00") {
            let event = Event::default().event("error").data(payload.to_string());
            Some((Ok(event), rx))
        } else {
            let event = Event::default().event("token").data(msg);
            Some((Ok(event), rx))
        }
    });

    Ok(Sse::new(sse_stream))
}

async fn chat_session(
    State(state): State<AppState>,
    Json(request): Json<ChatSessionRequest>,
) -> Result<Json<ChatSessionResponse>, WebError> {
    let agent = selected_agent(request.agent_id.as_deref())?;
    let provider = parse_provider(&request.provider)?;
    let model = blank_to_none(Some(request.model))
        .ok_or_else(|| AppError::InvalidInput("Model is required".to_string()))?;
    let session_key = web_session_key(agent.id, &provider.to_string(), &model);
    let session = if request.new_session {
        let session = state.sessions.create_session()?;
        state
            .sessions
            .save_session(&session_key, &session.id, &session.messages)?;
        state.sessions.save_metrics(&session.id, &session.metrics)?;
        state
            .sessions
            .save_memory(&session.id, &AgentMemory::default())?;
        session
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    Ok(Json(ChatSessionResponse {
        agent_id: agent.id.to_string(),
        session_id: session.id,
        messages: session.messages,
        metrics: session.metrics,
    }))
}

#[derive(Debug, Serialize)]
struct AgentsResponse {
    agents: Vec<AgentView>,
}

#[derive(Debug, Serialize)]
struct AgentView {
    id: String,
    name: String,
    history_storage: String,
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
struct TokenStatusResponse {
    saved: bool,
}

#[derive(Debug, Serialize)]
struct ModelView {
    id: String,
    pricing: Option<ModelPricing>,
    pricing_source: Option<PricingResolution>,
    context_length: Option<u64>,
}

impl ModelView {
    fn from_model_info(
        model: crate::providers::ModelInfo,
        prices: &LiteLlmPriceCatalog,
        provider: ProviderKind,
    ) -> Self {
        let catalog_resolution = prices.resolve(provider, &model.id).ok().flatten();
        let pricing = model.pricing.clone().or_else(|| {
            catalog_resolution
                .as_ref()
                .map(|resolution| resolution.pricing.clone())
        });
        let context_length = model.context_length.or_else(|| {
            catalog_resolution
                .as_ref()
                .and_then(|resolution| resolution.context_length)
        });
        Self {
            id: model.id,
            pricing,
            pricing_source: catalog_resolution,
            context_length,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PricingResolveRequest {
    provider: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct PricingResolveResponse {
    pricing: Option<PricingResolution>,
}

#[derive(Debug, Deserialize)]
struct WebAttachment {
    name: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatWebRequest {
    agent_id: Option<String>,
    provider: String,
    base_url: String,
    token: String,
    token_provider: Option<String>,
    model: String,
    context_limit: Option<u32>,
    system_prompt: Option<String>,
    prompt: String,
    attachments: Option<Vec<WebAttachment>>,
    session_id: Option<String>,
    new_session: bool,
    messages: Option<Vec<ChatMessage>>,
    control: WebResponseControl,
    memory: Option<WebMemoryConfig>,
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
    agent_id: Option<String>,
    provider: String,
    model: String,
    session_id: Option<String>,
    new_session: bool,
}

#[derive(Debug, Serialize)]
struct ChatSessionResponse {
    agent_id: String,
    session_id: String,
    messages: Vec<ChatMessage>,
    metrics: crate::providers::RequestMetrics,
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

#[derive(Debug, Clone, Deserialize)]
struct WebMemoryConfig {
    strategy: Option<String>,
    recent_messages: Option<usize>,
    summarize_after_messages: Option<usize>,
    summary_chunk_messages: Option<usize>,
    summarize_at_context_percent: Option<u8>,
}

impl Default for WebMemoryConfig {
    fn default() -> Self {
        let defaults = MemoryConfig::default();
        Self {
            strategy: Some(defaults.strategy.to_string()),
            recent_messages: Some(defaults.recent_messages),
            summarize_after_messages: Some(defaults.summarize_after_messages),
            summary_chunk_messages: Some(defaults.summary_chunk_messages),
            summarize_at_context_percent: Some(defaults.summarize_at_context_percent),
        }
    }
}

impl WebMemoryConfig {
    fn into_memory_config(self) -> MemoryConfig {
        let defaults = MemoryConfig::default();
        MemoryConfig {
            strategy: match self.strategy.as_deref() {
                Some("full") => MemoryStrategy::Full,
                _ => MemoryStrategy::Summary,
            },
            recent_messages: self.recent_messages.unwrap_or(defaults.recent_messages),
            summarize_after_messages: self
                .summarize_after_messages
                .unwrap_or(defaults.summarize_after_messages),
            summary_chunk_messages: self
                .summary_chunk_messages
                .unwrap_or(defaults.summary_chunk_messages)
                .max(1),
            summarize_at_context_percent: self
                .summarize_at_context_percent
                .unwrap_or(defaults.summarize_at_context_percent)
                .clamp(1, 100),
        }
    }
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
                Some("toon") => ResponseFormat::Toon,
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
    agent_id: String,
    session_id: String,
    text: String,
    finish_reason: Option<String>,
    metrics: crate::providers::RequestMetrics,
    summary_metrics: Option<crate::providers::RequestMetrics>,
    session_metrics: crate::providers::RequestMetrics,
    messages: Vec<ChatMessage>,
    debug: ChatDebugView,
}

#[derive(Debug, Clone, Serialize)]
struct ChatDebugView {
    provider_request: HttpDebugRequest,
    provider_response: HttpDebugResponse,
}

fn build_web_prompt(prompt: &str, attachments: Option<&[WebAttachment]>) -> String {
    let Some(attachments) = attachments else {
        return prompt.to_string();
    };
    let non_empty: Vec<&WebAttachment> = attachments
        .iter()
        .filter(|a| !a.content.is_empty())
        .collect();
    if non_empty.is_empty() {
        return prompt.to_string();
    }
    let mut parts = vec![prompt.to_string()];
    for attachment in non_empty {
        parts.push(format!(
            "--- {} ---\n{}",
            attachment.name, attachment.content
        ));
    }
    parts.join("\n\n")
}

fn web_request_pricing(
    request: &ChatWebRequest,
    prices: &LiteLlmPriceCatalog,
    profile: &ProfileConfig,
) -> Option<ModelPricing> {
    request
        .pricing
        .clone()
        .and_then(WebPricing::into_model_pricing)
        .or_else(|| {
            prices
                .resolve(profile.provider, &profile.model)
                .ok()
                .flatten()
                .map(|resolution| resolution.pricing)
        })
}

fn web_request_context_limit(
    request: &ChatWebRequest,
    prices: &LiteLlmPriceCatalog,
    profile: &ProfileConfig,
) -> Option<u32> {
    request.context_limit.or_else(|| {
        prices
            .resolve(profile.provider, &profile.model)
            .ok()
            .flatten()
            .and_then(|resolution| resolution.context_length)
            .and_then(|limit| u32::try_from(limit).ok())
    })
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
            agent_id: Some(crate::chat::LOCAL_SESSION_AGENT_ID.to_string()),
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            context_limit: None,
            system_prompt: Some("Ты отвечаешь кратко.".to_string()),
            prompt: "Привет".to_string(),
            attachments: None,
            session_id: None,
            new_session: false,
            messages: None,
            control: WebResponseControl::default(),
            memory: None,
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
            agent_id: Some(crate::chat::LOCAL_SESSION_AGENT_ID.to_string()),
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            context_limit: None,
            system_prompt: Some("Ты отвечаешь кратко.".to_string()),
            prompt: "Продолжи".to_string(),
            attachments: None,
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
            memory: None,
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
            agent_id: Some(crate::chat::LOCAL_SESSION_AGENT_ID.to_string()),
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            context_limit: None,
            system_prompt: None,
            prompt: "Продолжи".to_string(),
            attachments: None,
            session_id: Some("session".to_string()),
            new_session: false,
            messages: Some(vec![ChatMessage {
                role: Role::User,
                content: "client".to_string(),
            }]),
            control: WebResponseControl::default(),
            memory: None,
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

    #[test]
    fn web_rejects_unknown_agent_id() {
        assert!(matches!(
            selected_agent(Some("unknown-agent")),
            Err(AppError::InvalidInput(_))
        ));
        assert!(selected_agent(Some(crate::chat::LOCAL_SESSION_AGENT_ID)).is_ok());
    }

    #[test]
    fn web_memory_config_maps_strategy_and_limits() {
        let config = WebMemoryConfig {
            strategy: Some("full".to_string()),
            recent_messages: Some(4),
            summarize_after_messages: Some(8),
            summary_chunk_messages: Some(0),
            summarize_at_context_percent: Some(95),
        }
        .into_memory_config();

        assert_eq!(config.strategy, MemoryStrategy::Full);
        assert_eq!(config.recent_messages, 4);
        assert_eq!(config.summarize_after_messages, 8);
        assert_eq!(config.summary_chunk_messages, 1);
        assert_eq!(config.summarize_at_context_percent, 95);
    }

    #[test]
    fn web_request_pricing_uses_official_deepseek_pricing_before_stale_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prices.json");
        std::fs::write(
            &path,
            r#"{
              "fetched_at_unix": 4102444800,
              "source_url": "https://example.test/catalog.json",
              "entries": {
                "deepseek-chat": {
                  "litellm_provider": "deepseek",
                  "input_cost_per_token": 0.00000028,
                  "output_cost_per_token": 0.00000042,
                  "cache_read_input_token_cost": 0.000000028,
                  "max_input_tokens": 131072,
                  "source": "https://example.test/deepseek"
                }
              }
            }"#,
        )
        .expect("write price cache");
        let prices = LiteLlmPriceCatalog::with_path(path);
        let request = ChatWebRequest {
            agent_id: Some(crate::chat::LOCAL_SESSION_AGENT_ID.to_string()),
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            context_limit: None,
            system_prompt: None,
            prompt: "Привет".to_string(),
            attachments: None,
            session_id: None,
            new_session: false,
            messages: None,
            control: WebResponseControl::default(),
            memory: None,
            pricing: None,
            billing: None,
        };
        let profile = request.profile().expect("profile");

        let pricing = web_request_pricing(&request, &prices, &profile).expect("pricing");

        assert!((pricing.input_per_million.unwrap() - 0.14).abs() < f64::EPSILON);
        assert!((pricing.output_per_million - 0.28).abs() < f64::EPSILON);
        assert!((pricing.cache_hit_input_per_million.unwrap() - 0.0028).abs() < 1e-12);
    }

    #[test]
    fn web_model_view_includes_official_deepseek_pricing_when_provider_models_are_bare() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prices.json");
        std::fs::write(
            &path,
            r#"{
              "fetched_at_unix": 4102444800,
              "source_url": "https://example.test/catalog.json",
              "entries": {
                "deepseek-ai/deepseek-chat": {
                  "litellm_provider": "deepseek-ai",
                  "input_cost_per_token": 0.00000028,
                  "output_cost_per_token": 0.00000042,
                  "max_input_tokens": 131072,
                  "source": "https://example.test/deepseek"
                }
              }
            }"#,
        )
        .expect("write price cache");
        let prices = LiteLlmPriceCatalog::with_path(path);
        let view = ModelView::from_model_info(
            crate::providers::ModelInfo {
                id: "deepseek-chat".to_string(),
                pricing: None,
                context_length: None,
            },
            &prices,
            ProviderKind::DeepSeek,
        );

        let pricing = view.pricing.expect("catalog pricing");
        assert!((pricing.input_per_million.unwrap() - 0.14).abs() < f64::EPSILON);
        assert!((pricing.output_per_million - 0.28).abs() < f64::EPSILON);
        assert_eq!(view.context_length, Some(1_000_000));
        assert_eq!(
            view.pricing_source.expect("pricing source").matched_model,
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn web_request_context_limit_prefers_request_then_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prices.json");
        std::fs::write(
            &path,
            r#"{
              "fetched_at_unix": 4102444800,
              "source_url": "https://example.test/catalog.json",
              "entries": {
                "deepseek-chat": {
                  "litellm_provider": "deepseek",
                  "output_cost_per_token": 0.00000042,
                  "max_input_tokens": 131072
                }
              }
            }"#,
        )
        .expect("write price cache");
        let prices = LiteLlmPriceCatalog::with_path(path);
        let request = ChatWebRequest {
            agent_id: Some(crate::chat::LOCAL_SESSION_AGENT_ID.to_string()),
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token: String::new(),
            token_provider: None,
            model: "deepseek-chat".to_string(),
            context_limit: Some(32_000),
            system_prompt: None,
            prompt: "Привет".to_string(),
            attachments: None,
            session_id: None,
            new_session: false,
            messages: None,
            control: WebResponseControl::default(),
            memory: None,
            pricing: None,
            billing: None,
        };
        let profile = request.profile().expect("profile");

        assert_eq!(
            web_request_context_limit(&request, &prices, &profile),
            Some(32_000)
        );

        let request_without_override = ChatWebRequest {
            context_limit: None,
            ..request
        };
        assert_eq!(
            web_request_context_limit(&request_without_override, &prices, &profile),
            Some(1_000_000)
        );
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

    #[test]
    fn web_ui_stream_done_updates_request_metrics_and_debug() {
        assert!(
            INDEX_HTML.contains("setMetrics(data.metrics, data.summary_metrics);"),
            "streaming done handler must render per-request metrics so request cost is not blank"
        );
        assert!(
            INDEX_HTML.contains("summary_metrics"),
            "streaming done handler must receive separate summary metrics"
        );
        assert!(
            INDEX_HTML.contains("setDebug(data.debug);"),
            "streaming done handler must replace temporary HTTP debug with provider JSON"
        );
        assert!(
            INDEX_HTML.contains("setSessionMetrics(data.session_metrics);"),
            "streaming done handler must keep cumulative session metrics visible"
        );
    }

    #[test]
    fn web_ui_labels_debug_response_as_raw_provider_body() {
        assert!(
            INDEX_HTML.contains("Ответ провайдера (raw)"),
            "debug response must be labeled as raw provider data, not app-calculated metrics"
        );
    }

    #[test]
    fn web_ui_composer_meta_only_shows_for_attachments() {
        let marker = "function updateComposerMeta()";
        let start = INDEX_HTML.find(marker).expect("updateComposerMeta");
        let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 320)];

        assert!(
            body.contains("pendingAttachments.length > 0"),
            "composer meta should be driven by real attachment chips"
        );
        assert!(
            !body.contains("contextWindowBadge"),
            "context badge must not force an empty blue composer meta rectangle above the chat"
        );
    }

    #[test]
    fn web_ui_session_metrics_include_cumulative_cost() {
        let marker = "function setSessionMetrics(metrics)";
        let start = INDEX_HTML.find(marker).expect("setSessionMetrics");
        let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 900)];

        assert!(
            body.contains("metrics.cost"),
            "session metrics should render accumulated cost, not only tokens"
        );
        assert!(
            body.contains("metric-line\"><em>cost</em>"),
            "session cost should be visible in the metrics panel"
        );
    }

    #[test]
    fn web_ui_displays_summary_metrics_separately() {
        assert!(INDEX_HTML.contains("Сжатие истории"));
        assert!(INDEX_HTML.contains("id=\"metricSummary\""));
        assert!(INDEX_HTML.contains("function summaryLines(metrics)"));
        assert!(
            INDEX_HTML.contains("не запускалось"),
            "UI should explicitly say when no history summary request happened"
        );
    }

    #[test]
    fn web_ui_context_settings_use_human_labels() {
        assert!(INDEX_HTML.contains("Как хранить историю"));
        assert!(INDEX_HTML.contains("Свежие сообщения без сжатия"));
        assert!(INDEX_HTML.contains("Начинать сжатие после"));
        assert!(INDEX_HTML.contains("Сжимать при заполнении контекста"));
        assert!(INDEX_HTML.contains("Размер порции summary"));
        assert!(
            !INDEX_HTML.contains(">memory_strategy<"),
            "context settings should not expose raw API field names as labels"
        );
    }

    #[test]
    fn web_ui_resolves_pricing_after_model_options_are_loaded() {
        let marker = "function setModelOptions(models, selectedModel)";
        let start = INDEX_HTML.find(marker).expect("setModelOptions");
        let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 1_400)];

        assert!(
            body.contains("resolveSelectedModelPricing();"),
            "selected model pricing must be resolved after loading model options, including DeepSeek models with bare /models metadata"
        );
    }

    #[test]
    fn web_ui_resolves_context_even_when_model_pricing_is_cached() {
        assert!(
            INDEX_HTML.contains("const needsPricing = !modelPricingById.has(model)"),
            "UI should track pricing resolution separately"
        );
        assert!(
            INDEX_HTML.contains("const needsContext = !modelContextById.has(model)"),
            "UI must still resolve context window when pricing is already cached"
        );
        assert!(
            INDEX_HTML
                .contains("if (needsPricing) modelPricingById.set(model, data.pricing.pricing);"),
            "catalog pricing should not overwrite manual/provider pricing while resolving context"
        );
    }
}
