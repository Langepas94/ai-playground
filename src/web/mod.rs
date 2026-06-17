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
    chat::memory::{MemoryConfig, MemoryLayer, MemoryStrategy},
    chat::{
        AgentMemory, ChatAgent, ConversationSession, LocalSessionStore, StatefulReport,
        add_request_metrics, available_agents, selected_agent, web_session_key,
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

mod agents;
mod error;
mod parameters;
mod tokens;
mod util;

#[cfg(test)]
use crate::chat::{SavedAgent, TaskContext, UserProfileBindings};
#[cfg(test)]
use agents::{
    AgentsManageRequest, TaskPayload, UserProfilesManageRequest, persist_agent_initial_context,
};
use agents::{agents_manage, user_profiles_manage};
use error::{ApiJson, WebError};
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

struct PreparedWebChat {
    agent_id: String,
    profile: ProfileConfig,
    token: String,
    prompt: String,
    session_key: String,
    session: ConversationSession,
    agent: ChatAgent,
}

pub async fn serve(addr: SocketAddr) -> Result<(), AppError> {
    let client = ReqwestProviderClient::new()?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sessions = LocalSessionStore::new()?;
    let prices = LiteLlmPriceCatalog::new()?;
    spawn_price_sync_task(client.clone(), prices.clone());
    let app = build_app(AppState {
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

fn build_app(state: AppState) -> Router {
    Router::new()
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
        .route("/api/memory/update", post(memory_update))
        .route("/api/agents/manage", post(agents_manage))
        .route("/api/user-profiles/manage", post(user_profiles_manage))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)) // 50 MB — для вложений
        .with_state(state)
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
    ApiJson(request): ApiJson<ModelsRequest>,
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
    ApiJson(request): ApiJson<PricingResolveRequest>,
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
    ApiJson(request): ApiJson<ModelsRequest>,
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
    ApiJson(request): ApiJson<ModelsRequest>,
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

async fn prepare_web_chat(
    state: &AppState,
    request: &ChatWebRequest,
) -> Result<PreparedWebChat, WebError> {
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
    let session_key = effective_session_key(
        request.saved_agent_id.as_deref(),
        &web_session_key(agent_spec.id, &profile.provider.to_string(), &profile.model),
    );
    let session = if request.new_session {
        state.sessions.create_session()?
    } else {
        match request.session_id.as_deref().and_then(blank_str_to_none) {
            Some(session_id) => state.sessions.load_session(session_id)?,
            None => state.sessions.load_or_create_latest(&session_key)?,
        }
    };
    let mut memory = state.sessions.load_memory(&session.id)?;
    state.sessions.seed_long_term(&session_key, &mut memory)?;
    let memory_config = request
        .memory
        .clone()
        .unwrap_or_default()
        .into_memory_config();
    let _ = state.prices.sync_if_stale(state.client.http_client()).await;
    let pricing = web_request_pricing(request, &state.prices, &profile);
    let context_limit = web_request_context_limit(request, &state.prices, &profile);
    let mut agent = ChatAgent::new(
        profile.clone(),
        token.clone(),
        request.initial_history(session.messages.clone()),
        memory,
        request.control.clone().into_control(),
        pricing,
        request
            .billing
            .clone()
            .and_then(WebBilling::into_billing_lookup),
    );
    agent.set_memory_config(memory_config);
    agent.set_context_limit(context_limit);
    agent.set_topic_store(Some(state.sessions.topic_file_storage(&session.id)?));
    apply_saved_agent_memory(
        &state.sessions,
        request.saved_agent_id.as_deref(),
        session.id.as_str(),
        &mut agent,
    )?;
    apply_runtime_user_profile(
        &state.sessions,
        request.user_profile_id.as_deref(),
        request.saved_agent_id.as_deref(),
        &mut agent,
    )?;
    Ok(PreparedWebChat {
        agent_id: agent_spec.id.to_string(),
        profile,
        token,
        prompt,
        session_key,
        session,
        agent,
    })
}

async fn chat(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ChatWebRequest>,
) -> Result<Json<ChatWebResponse>, WebError> {
    let PreparedWebChat {
        agent_id,
        prompt,
        session_key,
        session,
        mut agent,
        ..
    } = prepare_web_chat(&state, &request).await?;
    let (response, provider_debug, mut context_metrics) = agent
        .respond_with_debug_and_context_metrics(&state.client, prompt.clone())
        .await?;
    // Stateful post-processing: fill profile, advance task stage, check invariants.
    let (agent_state, stateful_metrics) = run_and_persist_stateful(
        &state.sessions,
        request.saved_agent_id.as_deref(),
        session.id.as_str(),
        &mut agent,
        &state.client,
        &prompt,
        &response.text,
    )
    .await;
    let mut response = response;
    if let Some(metrics) = &stateful_metrics {
        response.metrics = add_request_metrics(&response.metrics, metrics);
        context_metrics = Some(match context_metrics {
            Some(current) => add_request_metrics(&current, metrics),
            None => metrics.clone(),
        });
    }
    let session_metrics = add_request_metrics(&session.metrics, &response.metrics);
    state
        .sessions
        .save_session(&session_key, &session.id, agent.history())?;
    state.sessions.save_metrics(&session.id, &session_metrics)?;
    state.sessions.save_memory(&session.id, agent.memory())?;
    state
        .sessions
        .save_long_term(&session_key, agent.memory())?;
    let context_debug =
        build_context_debug(agent.memory(), &agent.memory_config(), agent.history());
    Ok(Json(ChatWebResponse {
        agent_id,
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
        agent_state,
    }))
}

async fn chat_stream(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ChatWebRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, WebError> {
    let PreparedWebChat {
        profile,
        token,
        prompt,
        session_key,
        session,
        mut agent,
        ..
    } = prepare_web_chat(&state, &request).await?;
    let prepared_stream = agent.prepare_stream_request(&state.client, &prompt).await?;
    let session_id = session.id.clone();
    let session_metrics_before = session.metrics.clone();

    let (tx, rx) = mpsc::unbounded_channel::<String>();
    if let Some(local_response) = prepared_stream.local_response {
        let session_metrics = add_request_metrics(&session_metrics_before, &local_response.metrics);
        state
            .sessions
            .save_session(&session_key, &session_id, agent.history())?;
        state.sessions.save_metrics(&session_id, &session_metrics)?;
        state.sessions.save_memory(&session_id, agent.memory())?;
        state
            .sessions
            .save_long_term(&session_key, agent.memory())?;
        let context_debug =
            build_context_debug(agent.memory(), &agent.memory_config(), agent.history());
        let done_event = serde_json::json!({
            "done": true,
            "session_id": session_id,
            "metrics": local_response.metrics,
            "context_metrics": prepared_stream.context_metrics,
            "session_metrics": session_metrics,
            "messages": agent.history(),
            "context_debug": context_debug,
            "debug": ChatDebugView {
                provider_request: crate::providers::HttpDebugRequest {
                    method: "LOCAL".to_string(),
                    url: "local://topic-file-routing/not-found".to_string(),
                    headers: Default::default(),
                    body: serde_json::json!({}),
                },
                provider_response: crate::providers::HttpDebugResponse {
                    status: 200,
                    headers: Default::default(),
                    body: serde_json::json!({ "finish_reason": "topic_not_found" }),
                },
            },
        });
        let _ = tx.send(local_response.text);
        let _ = tx.send(format!("\x00DONE\x00{done_event}"));
        drop(tx);
        return Ok(Sse::new(sse_event_stream(rx)));
    }
    let chat_request = prepared_stream
        .request
        .ok_or_else(|| AppError::InvalidInput("Stream request was not prepared".to_string()))?;
    let context_metrics = prepared_stream.context_metrics;
    let client = state.client.clone();
    let sessions = state.sessions.clone();
    let saved_agent_id = request.saved_agent_id.clone();
    let stateful_session_id = session_id.clone();

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
                agent.record_stream_response(prompt.clone(), assistant_text.clone());
                // Stateful post-processing on the completed turn.
                let (agent_state, stateful_metrics) = run_and_persist_stateful(
                    &sessions,
                    saved_agent_id.as_deref(),
                    stateful_session_id.as_str(),
                    &mut agent,
                    &client,
                    &prompt,
                    &assistant_text,
                )
                .await;
                let mut response_metrics = response.metrics.clone();
                let mut context_metrics = context_metrics.clone();
                if let Some(context_metrics) = &context_metrics {
                    response_metrics = add_request_metrics(&response_metrics, context_metrics);
                }
                if let Some(metrics) = &stateful_metrics {
                    response_metrics = add_request_metrics(&response_metrics, metrics);
                    context_metrics = Some(match context_metrics {
                        Some(current) => add_request_metrics(&current, metrics),
                        None => metrics.clone(),
                    });
                }
                let session_metrics =
                    add_request_metrics(&session_metrics_before, &response_metrics);
                let _ = sessions.save_session(&session_key, &session_id, agent.history());
                let _ = sessions.save_metrics(&session_id, &session_metrics);
                let _ = sessions.save_memory(&session_id, agent.memory());
                let _ = sessions.save_long_term(&session_key, agent.memory());
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
                    "agent_state": agent_state,
                });
                let _ = tx.send(format!("\x00DONE\x00{done_event}"));
                drop(tx);
            }
            Err(err) => {
                let _ = tx.send(format!("\x00ERR\x00{err}"));
            }
        }
    });

    Ok(Sse::new(sse_event_stream(rx)))
}

fn sse_event_stream(
    rx: mpsc::UnboundedReceiver<String>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(rx, |mut rx| async move {
        let msg = rx.recv().await?;
        if let Some(payload) = msg.strip_prefix("\x00DONE\x00") {
            let event = Event::default().event("done").data(payload);
            Some((Ok(event), rx))
        } else if let Some(payload) = msg.strip_prefix("\x00ERR\x00") {
            let event = Event::default().event("error").data(payload);
            Some((Ok(event), rx))
        } else {
            let event = Event::default().event("token").data(msg);
            Some((Ok(event), rx))
        }
    })
}

async fn chat_session(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ChatSessionRequest>,
) -> Result<Json<ChatSessionResponse>, WebError> {
    let agent = selected_agent(request.agent_id.as_deref())?;
    let provider = parse_provider(&request.provider)?;
    let model = blank_to_none(Some(request.model))
        .ok_or_else(|| AppError::InvalidInput("Model is required".to_string()))?;
    // For a saved agent, key sessions on `agent:<id>` so chat_session, chat, and
    // the dialog index all share one key (consistent dialog list per agent).
    let session_key = effective_session_key(
        request.saved_agent_id.as_deref(),
        &web_session_key(agent.id, &provider.to_string(), &model),
    );
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

/// Session key for a request: a stable `agent:<id>` key when a saved agent is
/// active, otherwise the provider/model-derived key.
fn effective_session_key(saved_agent_id: Option<&str>, fallback: &str) -> String {
    match saved_agent_id.and_then(blank_str_to_none) {
        Some(id) => format!("agent:{id}"),
        None => fallback.to_string(),
    }
}

/// Load the saved agent's stateful layers (dialog task + stage, profile, invariants,
/// domain) and attach them so they are injected into the next request and so the
/// stateful post-processing can mutate + persist them.
fn apply_saved_agent_memory(
    store: &LocalSessionStore,
    saved_agent_id: Option<&str>,
    session_id: &str,
    agent: &mut ChatAgent,
) -> Result<(), AppError> {
    if let Some(id) = saved_agent_id.and_then(blank_str_to_none) {
        agent.set_task_state(Some(store.load_dialog_task(id, session_id)?));
        agent.set_agent_profile(Some(store.load_profile(id)?));
        if let Some(saved) = store.load_agent(id)? {
            agent.set_invariants(saved.invariants);
            agent.set_domain(saved.domain);
        }
    }
    Ok(())
}

fn apply_runtime_user_profile(
    store: &LocalSessionStore,
    explicit_profile_id: Option<&str>,
    saved_agent_id: Option<&str>,
    agent: &mut ChatAgent,
) -> Result<(), AppError> {
    if explicit_profile_id == Some("__none__") {
        agent.set_user_profile(None);
        return Ok(());
    }
    let profile = store.resolve_user_profile(explicit_profile_id, saved_agent_id)?;
    agent.set_user_profile(profile);
    Ok(())
}

/// After a turn, run the agent's stateful post-processing and persist any changes
/// to its working (task + stage) and long-term (profile) layers. Returns a debug
/// view of what happened, plus the auxiliary token metrics.
async fn run_and_persist_stateful(
    store: &LocalSessionStore,
    saved_agent_id: Option<&str>,
    session_id: &str,
    agent: &mut ChatAgent,
    client: &dyn crate::providers::ProviderClient,
    user_prompt: &str,
    answer: &str,
) -> (
    Option<StatefulDebugView>,
    Option<crate::providers::RequestMetrics>,
) {
    let Some(id) = saved_agent_id.and_then(blank_str_to_none) else {
        return (None, None);
    };
    let report = agent
        .stateful_postprocess(client, user_prompt, answer)
        .await;
    if let Some(task) = agent.task_state() {
        let _ = store.save_dialog_task(id, session_id, task);
    }
    if let Some(profile) = agent.agent_profile() {
        let _ = store.save_profile(id, profile);
    }
    let metrics = report.metrics.clone();
    (Some(stateful_debug_view(&report)), metrics)
}

fn stateful_debug_view(report: &StatefulReport) -> StatefulDebugView {
    StatefulDebugView {
        stage: report.stage.map(|stage| stage.to_string()),
        current_step: report.current_step.clone(),
        expected_action: report.expected_action.clone(),
        paused: report.paused,
        resume_hint: report.resume_hint.clone(),
        pending_questions: report.pending_questions.clone(),
        violations: report.violations.clone(),
        stage_transition: report
            .stage_transition
            .map(|transition| StageTransitionView {
                from: transition.from.to_string(),
                to: transition.to.to_string(),
                accepted: transition.accepted,
            }),
    }
}

async fn memory_update(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<MemoryUpdateRequest>,
) -> Result<Json<MemoryUpdateResponse>, WebError> {
    let agent = selected_agent(request.agent_id.as_deref())?;
    let provider = parse_provider(&request.provider)?;
    let model = blank_to_none(Some(request.model.clone()))
        .ok_or_else(|| AppError::InvalidInput("Model is required".to_string()))?;
    let session_key = effective_session_key(
        request.saved_agent_id.as_deref(),
        &web_session_key(agent.id, &provider.to_string(), &model),
    );
    let session = match request.session_id.as_deref().and_then(blank_str_to_none) {
        Some(session_id) => state.sessions.load_session(session_id)?,
        None => state.sessions.load_or_create_latest(&session_key)?,
    };
    let mut memory = state.sessions.load_memory(&session.id)?;
    state.sessions.seed_long_term(&session_key, &mut memory)?;
    let memory_config = request
        .memory
        .clone()
        .unwrap_or_default()
        .into_memory_config();

    match request.action.as_str() {
        "set" => {
            let key = request.key.clone().unwrap_or_default().trim().to_string();
            let value = request.value.clone().unwrap_or_default().trim().to_string();
            if key.is_empty() || value.is_empty() {
                return Err(
                    AppError::InvalidInput("Key and value are required".to_string()).into(),
                );
            }
            let layer = request
                .layer
                .as_deref()
                .unwrap_or("long-term")
                .parse::<MemoryLayer>()
                .map_err(|_| AppError::InvalidInput("Unknown memory layer".to_string()))?;
            if layer == MemoryLayer::ShortTerm {
                return Err(AppError::InvalidInput(
                    "Short-term layer holds the dialog window, not KV facts".to_string(),
                )
                .into());
            }
            if layer == MemoryLayer::LongTerm
                && (crate::chat::memory::looks_sensitive(&key)
                    || crate::chat::memory::looks_sensitive(&value))
            {
                return Err(AppError::InvalidInput(
                    "Sensitive values must not be stored in long-term memory".to_string(),
                )
                .into());
            }
            memory.set_fact_in_layer(key, value, layer);
        }
        "delete" => {
            let key = request.key.clone().unwrap_or_default();
            memory.remove_fact(key.trim());
        }
        "clear-layer" => {
            let layer = request
                .layer
                .as_deref()
                .unwrap_or_default()
                .parse::<MemoryLayer>()
                .map_err(|_| AppError::InvalidInput("Unknown memory layer".to_string()))?;
            memory.clear_layer(layer);
        }
        other => {
            return Err(AppError::InvalidInput(format!("Unknown memory action: {other}")).into());
        }
    }

    state.sessions.save_memory(&session.id, &memory)?;
    state.sessions.save_long_term(&session_key, &memory)?;

    let context_debug = build_context_debug(&memory, &memory_config, &session.messages);
    Ok(Json(MemoryUpdateResponse {
        session_id: session.id,
        context_debug,
    }))
}

#[derive(Debug, Deserialize)]
struct MemoryUpdateRequest {
    agent_id: Option<String>,
    saved_agent_id: Option<String>,
    provider: String,
    model: String,
    session_id: Option<String>,
    action: String,
    key: Option<String>,
    value: Option<String>,
    layer: Option<String>,
    memory: Option<WebMemoryConfig>,
}

#[derive(Debug, Serialize)]
struct MemoryUpdateResponse {
    session_id: String,
    context_debug: ContextDebugView,
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
    #[serde(default)]
    saved_agent_id: Option<String>,
    #[serde(default)]
    user_profile_id: Option<String>,
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
    #[serde(default)]
    saved_agent_id: Option<String>,
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
    topic_file_routing: Option<bool>,
    topic_drift_guard: Option<bool>,
    topic_auto_create: Option<bool>,
    topic_classifier_prompt: Option<String>,
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
            topic_file_routing: Some(defaults.topic_file_routing),
            topic_drift_guard: Some(defaults.topic_drift_guard),
            topic_auto_create: Some(defaults.topic_auto_create),
            topic_classifier_prompt: Some(defaults.topic_classifier_prompt),
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
            topic_file_routing: self
                .topic_file_routing
                .unwrap_or(defaults.topic_file_routing),
            topic_drift_guard: self.topic_drift_guard.unwrap_or(defaults.topic_drift_guard),
            topic_auto_create: self.topic_auto_create.unwrap_or(defaults.topic_auto_create),
            topic_classifier_prompt: blank_to_none(self.topic_classifier_prompt)
                .unwrap_or(defaults.topic_classifier_prompt),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_state: Option<StatefulDebugView>,
}

#[derive(Debug, Clone, Serialize)]
struct ContextDebugView {
    strategy: String,
    facts: FactsDebugView,
    layers: MemoryLayersDebugView,
    active_topic: String,
    scoped_auto_route: bool,
    scoped_topics: Vec<ScopedTopicDebugView>,
}

/// Explicit 3-layer memory model for UI debug + control.
#[derive(Debug, Clone, Serialize)]
struct MemoryLayersDebugView {
    short_term: ShortTermLayerView,
    working: LayerFactsView,
    long_term: LayerFactsView,
}

/// Краткосрочная: текущий диалог (окно сообщений + session summary). Эфемерно.
#[derive(Debug, Clone, Serialize)]
struct ShortTermLayerView {
    session_summary: Option<String>,
    summarized_message_count: usize,
    recent_window: usize,
    recent_messages_sent: usize,
}

/// Рабочая / Долговременная: набор KV-фактов слоя.
#[derive(Debug, Clone, Serialize)]
struct LayerFactsView {
    facts: Vec<FactDebugView>,
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
    layer: String,
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

/// Stateful-agent view: current task stage, pending interview questions, the
/// stage transition decided this turn, and any invariant violations.
#[derive(Debug, Clone, Default, Serialize)]
struct StatefulDebugView {
    stage: Option<String>,
    current_step: String,
    expected_action: String,
    paused: bool,
    resume_hint: String,
    pending_questions: Vec<String>,
    violations: Vec<String>,
    stage_transition: Option<StageTransitionView>,
}

#[derive(Debug, Clone, Serialize)]
struct StageTransitionView {
    from: String,
    to: String,
    accepted: bool,
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
            layer: memory.fact_layer(key).to_string(),
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
    let layer_facts = |layer: crate::chat::memory::MemoryLayer| LayerFactsView {
        facts: memory
            .facts_in_layer(layer)
            .into_iter()
            .map(|(key, value)| FactDebugView {
                layer: layer.to_string(),
                key: key.to_string(),
                value: value.to_string(),
            })
            .collect(),
    };
    let non_system_count = history
        .iter()
        .filter(|message| message.role != Role::System)
        .count();
    let layers = MemoryLayersDebugView {
        short_term: ShortTermLayerView {
            session_summary: memory.session_summary.clone(),
            summarized_message_count: memory.summarized_message_count,
            recent_window: config.recent_messages,
            recent_messages_sent: non_system_count.min(config.recent_messages),
        },
        working: layer_facts(crate::chat::memory::MemoryLayer::Working),
        long_term: layer_facts(crate::chat::memory::MemoryLayer::LongTerm),
    };
    ContextDebugView {
        strategy: config.strategy.to_string(),
        layers,
        facts: FactsDebugView {
            persisted,
            extraction_prompt: if config.strategy == MemoryStrategy::StickyFacts {
                Some(config.facts_extraction_prompt.clone())
            } else {
                None
            },
            request_block: memory.facts_block(config.facts_prompt.as_str()),
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
