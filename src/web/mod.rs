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
    let (response, provider_debug, context_metrics) = agent
        .respond_with_debug_and_context_metrics(&state.client, prompt)
        .await?;
    let session_metrics = add_request_metrics(&session.metrics, &response.metrics);
    state
        .sessions
        .save_session(&session_key, &session.id, agent.history())?;
    state.sessions.save_metrics(&session.id, &session_metrics)?;
    state.sessions.save_memory(&session.id, agent.memory())?;
    let context_debug =
        build_context_debug(agent.memory(), &agent.memory_config(), agent.history());
    Ok(Json(ChatWebResponse {
        agent_id: agent_spec.id.to_string(),
        session_id: session.id,
        text: response.text,
        finish_reason: response.finish_reason,
        metrics: response.metrics,
        context_metrics,
        session_metrics,
        messages: agent.history().to_vec(),
        context_debug,
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
    let (chat_request, context_metrics) =
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
                let context_metrics = context_metrics.clone();
                if let Some(context_metrics) = &context_metrics {
                    response_metrics = add_request_metrics(&response_metrics, context_metrics);
                }
                let session_metrics =
                    add_request_metrics(&session_metrics_before, &response_metrics);
                agent.record_stream_response(prompt, assistant_text);
                let _ = sessions.save_session(&session_key, &session_id, agent.history());
                let _ = sessions.save_metrics(&session_id, &session_metrics);
                let _ = sessions.save_memory(&session_id, agent.memory());
                let context_debug =
                    build_context_debug(agent.memory(), &agent.memory_config(), agent.history());
                let done_event = serde_json::json!({
                    "done": true,
                    "session_id": session_id,
                    "metrics": response_metrics,
                    "context_metrics": context_metrics,
                    "session_metrics": session_metrics,
                    "messages": agent.history(),
                    "context_debug": context_debug,
                    "debug": ChatDebugView {
                        provider_request: provider_debug.request,
                        provider_response: provider_debug.response,
                    },
                });
                let _ = tx.send(format!("\x00DONE\x00{done_event}"));
                drop(tx);
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
    summary_prompt: Option<String>,
    facts_extraction_prompt: Option<String>,
    facts_prompt: Option<String>,
    active_branch: Option<String>,
    scoped_auto_route: Option<bool>,
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
            summary_prompt: Some(defaults.summary_prompt),
            facts_extraction_prompt: Some(defaults.facts_extraction_prompt),
            facts_prompt: Some(defaults.facts_prompt),
            active_branch: Some(defaults.active_branch),
            scoped_auto_route: Some(defaults.scoped_auto_route),
        }
    }
}

impl WebMemoryConfig {
    fn into_memory_config(self) -> MemoryConfig {
        let defaults = MemoryConfig::default();
        MemoryConfig {
            strategy: match self.strategy.as_deref() {
                Some("summary") => MemoryStrategy::Summary,
                Some("sticky-facts") => MemoryStrategy::StickyFacts,
                Some("branching") => MemoryStrategy::Branching,
                Some("scoped-branches") => MemoryStrategy::ScopedBranches,
                _ => MemoryStrategy::SlidingWindow,
            },
            recent_messages: self.recent_messages.unwrap_or(defaults.recent_messages),
            summarize_after_messages: self
                .summarize_after_messages
                .unwrap_or(defaults.summarize_after_messages),
            summary_chunk_messages: self
                .summary_chunk_messages
                .unwrap_or(defaults.summary_chunk_messages),
            summarize_at_context_percent: self
                .summarize_at_context_percent
                .unwrap_or(defaults.summarize_at_context_percent),
            summary_prompt: blank_to_none(self.summary_prompt).unwrap_or(defaults.summary_prompt),
            facts_extraction_prompt: blank_to_none(self.facts_extraction_prompt)
                .unwrap_or(defaults.facts_extraction_prompt),
            facts_prompt: blank_to_none(self.facts_prompt).unwrap_or(defaults.facts_prompt),
            active_branch: blank_to_none(self.active_branch).unwrap_or(defaults.active_branch),
            scoped_auto_route: self.scoped_auto_route.unwrap_or(defaults.scoped_auto_route),
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
    context_metrics: Option<crate::providers::RequestMetrics>,
    session_metrics: crate::providers::RequestMetrics,
    messages: Vec<ChatMessage>,
    context_debug: ContextDebugView,
    debug: ChatDebugView,
}

#[derive(Debug, Clone, Serialize)]
struct ContextDebugView {
    strategy: String,
    facts: FactsDebugView,
    active_topic: String,
    scoped_auto_route: bool,
    scoped_topics: Vec<ScopedTopicDebugView>,
}

#[derive(Debug, Clone, Serialize)]
struct FactsDebugView {
    persisted: Vec<FactDebugView>,
    extraction_prompt: Option<String>,
    request_block: Option<String>,
    recent_messages_sent: usize,
}

#[derive(Debug, Clone, Serialize)]
struct FactDebugView {
    key: String,
    value: String,
}

#[derive(Debug, Clone, Serialize)]
struct ScopedTopicDebugView {
    name: String,
    message_count: usize,
    active: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ChatDebugView {
    provider_request: HttpDebugRequest,
    provider_response: HttpDebugResponse,
}

fn build_context_debug(
    memory: &crate::chat::AgentMemory,
    config: &MemoryConfig,
    history: &[ChatMessage],
) -> ContextDebugView {
    let active_topic = config.active_branch.trim();
    let active_topic = if active_topic.is_empty() {
        "default"
    } else {
        active_topic
    };
    let scoped_topics = memory
        .branch_message_counts(history, active_topic)
        .into_iter()
        .map(|(name, message_count)| ScopedTopicDebugView {
            active: name == active_topic,
            name,
            message_count,
        })
        .collect();
    let persisted = memory
        .facts
        .iter()
        .map(|(key, value)| FactDebugView {
            key: key.clone(),
            value: value.clone(),
        })
        .collect::<Vec<_>>();
    let recent_messages_sent = if config.strategy == MemoryStrategy::StickyFacts {
        history
            .iter()
            .filter(|message| message.role != Role::System)
            .count()
            .min(config.recent_messages)
    } else {
        0
    };
    ContextDebugView {
        strategy: config.strategy.to_string(),
        facts: FactsDebugView {
            persisted,
            extraction_prompt: if config.strategy == MemoryStrategy::StickyFacts {
                Some(config.facts_extraction_prompt.clone())
            } else {
                None
            },
            request_block: if config.strategy == MemoryStrategy::StickyFacts {
                memory.facts_block(config.facts_prompt.as_str())
            } else {
                None
            },
            recent_messages_sent,
        },
        active_topic: active_topic.to_string(),
        scoped_auto_route: config.scoped_auto_route,
        scoped_topics,
    }
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
mod tests;
