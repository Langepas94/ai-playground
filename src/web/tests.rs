use super::*;
use crate::chat::{
    ProfileField, TaskArtifact, TaskPauseReason, TaskPipelineStage, TaskStage, TaskWorkerAgent,
};
use crate::secrets::MemorySecretStore;
use crate::web::agents::{ProfileFieldPayload, merge_profile_schema};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

fn web_profile(provider: ProviderKind) -> ProfileConfig {
    ProfileConfig {
        provider,
        model: provider.default_model().to_string(),
        base_url: provider.default_base_url().to_string(),
        token_ref: String::new(),
    }
}

/// Test wrapper for `apply_saved_agent_memory` with default profile/token/secrets.
fn apply_saved_for_test(
    store: &LocalSessionStore,
    id: Option<&str>,
    dialog: &str,
    agent: &mut ChatAgent,
) -> Result<(), crate::errors::AppError> {
    apply_saved_agent_memory(
        store,
        id,
        dialog,
        agent,
        &web_profile(ProviderKind::OpenAiCompatible),
        "test-token",
        &MemorySecretStore::default(),
    )
}

#[derive(Debug, Default)]
struct StatefulFakeClient {
    replies: std::sync::Mutex<Vec<String>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl StatefulFakeClient {
    fn with_replies(replies: Vec<&str>) -> Self {
        Self {
            replies: std::sync::Mutex::new(
                replies.into_iter().map(ToString::to_string).rev().collect(),
            ),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::providers::ProviderClient for StatefulFakeClient {
    async fn list_models(
        &self,
        _profile: &ProfileConfig,
        _token: &str,
    ) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    async fn chat_completion(
        &self,
        _profile: &ProfileConfig,
        _token: &str,
        _request: crate::providers::ChatRequest,
    ) -> Result<crate::providers::ChatResponse, AppError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let text = self
            .replies
            .lock()
            .expect("replies")
            .pop()
            .unwrap_or_else(|| r#"{"stage":"clarify","reason":"stay"}"#.to_string());
        Ok(crate::providers::ChatResponse {
            text,
            finish_reason: Some("stop".to_string()),
            metrics: crate::providers::RequestMetrics {
                elapsed_ms: 1,
                usage: None,
                cost: None,
            },
        })
    }

    async fn chat_completion_with_debug(
        &self,
        profile: &ProfileConfig,
        token: &str,
        request: crate::providers::ChatRequest,
    ) -> Result<
        (
            crate::providers::ChatResponse,
            crate::providers::ProviderExchangeDebug,
        ),
        AppError,
    > {
        let response = self.chat_completion(profile, token, request).await?;
        Ok((
            response,
            crate::providers::ProviderExchangeDebug {
                request: crate::providers::HttpDebugRequest {
                    method: "POST".to_string(),
                    url: "https://example.test/v1/chat/completions".to_string(),
                    headers: Default::default(),
                    body: serde_json::json!({}),
                },
                response: crate::providers::HttpDebugResponse {
                    status: 200,
                    headers: Default::default(),
                    body: serde_json::json!({}),
                },
            },
        ))
    }
}

fn web_test_agent(id: &str) -> SavedAgent {
    SavedAgent {
        id: id.to_string(),
        name: "Menu helper".to_string(),
        provider: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        domain: "restaurant menu helper".to_string(),
        ..SavedAgent::default()
    }
}

fn web_test_chat_agent() -> ChatAgent {
    ChatAgent::new(
        web_profile(ProviderKind::OpenAiCompatible),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    )
}

fn web_test_state(store: LocalSessionStore) -> AppState {
    AppState {
        client: ReqwestProviderClient::new().expect("client"),
        secrets: std::sync::Arc::new(MemorySecretStore::default()),
        sessions: store,
        prices: LiteLlmPriceCatalog::new().expect("prices"),
    }
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&body).expect("json response")
}

#[tokio::test]
async fn router_json_rejections_use_web_error_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = build_app(web_test_state(LocalSessionStore::from_root(
        dir.path().join("sessions"),
    )));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"provider":"deepseek","base_url":"https://api.deepseek.com","token":"","model":"deepseek-chat","prompt":""}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let body = response_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("Invalid JSON body"),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn router_exposes_documented_web_api_surface() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = build_app(web_test_state(LocalSessionStore::from_root(
        dir.path().join("sessions"),
    )));

    let providers = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/providers")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("providers");
    assert_eq!(providers.status(), StatusCode::OK);

    let pricing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/pricing/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("pricing status");
    assert_eq!(pricing.status(), StatusCode::OK);

    for path in ["/api/agent/session", "/api/chat/session"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"provider":"deepseek","model":"deepseek-chat","new_session":true}"#,
                    ))
                    .expect("request"),
            )
            .await
            .unwrap_or_else(|error| panic!("{path} failed: {error}"));
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = response_json(response).await;
        assert!(body["session_id"].as_str().is_some(), "{path}: {body}");
    }
}

#[tokio::test]
async fn user_profiles_manage_roundtrips_over_http_router() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = build_app(web_test_state(LocalSessionStore::from_root(
        dir.path().join("sessions"),
    )));
    let profile = serde_json::json!({
        "id": "chef-short",
        "display_name": "Chef short",
        "style_preferences": ["concise"],
        "language_preferences": ["English"],
        "custom_instructions": "Prefer culinary examples"
    });

    let save = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user-profiles/manage")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"action":"save","profile":profile}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("save");
    assert_eq!(save.status(), StatusCode::OK);
    let save_body = response_json(save).await;
    assert_eq!(save_body["profile"]["id"], "chef-short");

    let load = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user-profiles/manage")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"action":"load","id":"chef-short"}"#))
                .expect("request"),
        )
        .await
        .expect("load");
    assert_eq!(load.status(), StatusCode::OK);
    let load_body = response_json(load).await;
    assert_eq!(load_body["profile"]["display_name"], "Chef short");

    let delete = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user-profiles/manage")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"action":"delete","id":"chef-short"}"#))
                .expect("request"),
        )
        .await
        .expect("delete");
    assert_eq!(delete.status(), StatusCode::OK);
    let delete_body = response_json(delete).await;
    assert_eq!(
        delete_body["profiles"].as_array().expect("profiles").len(),
        0
    );
}

#[tokio::test]
async fn user_profiles_bind_can_clear_active_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_user_profile_bindings(&UserProfileBindings {
            active_profile_id: "artem-short-russian".to_string(),
            ..UserProfileBindings::default()
        })
        .expect("save bindings");
    let state = web_test_state(store);

    let result = user_profiles_manage(
        State(state),
        ApiJson(UserProfilesManageRequest {
            action: "bind".to_string(),
            active_profile_id: Some(String::new()),
            id: None,
            profile: None,
            agent_id: None,
            default_profile_id: None,
        }),
    )
    .await;
    let Json(response) = match result {
        Ok(response) => response,
        Err(_) => panic!("bind should succeed"),
    };

    assert!(
        response.bindings.active_profile_id.is_empty(),
        "clearing the UI selection must not keep injecting the old active profile"
    );
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

    let token =
        resolve_web_token(&secrets, &profile, " kimi-token ", Some("kimi")).expect("resolve token");

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
        saved_agent_id: None,
        user_profile_id: None,
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
        saved_agent_id: None,
        user_profile_id: None,
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
        saved_agent_id: None,
        user_profile_id: None,
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
fn saved_agent_uses_agent_scoped_session_key_for_chat_and_manual_memory() {
    assert_eq!(
        effective_session_key(Some("menu-agent"), "web:local:deepseek:deepseek-chat"),
        "agent:menu-agent"
    );
    assert_eq!(
        effective_session_key(None, "web:local:deepseek:deepseek-chat"),
        "web:local:deepseek:deepseek-chat"
    );
    assert_eq!(
        effective_session_key(Some("   "), "web:local:deepseek:deepseek-chat"),
        "web:local:deepseek:deepseek-chat"
    );
}

#[test]
fn saved_agent_loads_shared_feature_task_across_dialogs_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("menu-agent"))
        .expect("save agent");
    store
        .save_dialog_task(
            "menu-agent",
            "naming-dialog",
            &TaskContext {
                title: "name pizzeria".to_string(),
                goal: "invent pizzeria name".to_string(),
                results: vec!["Approved decision: ВолгаПицца".to_string()],
                ..TaskContext::default()
            },
        )
        .expect("save shared task");

    let mut agent = web_test_chat_agent();
    apply_saved_for_test(&store, Some("menu-agent"), "slogan-dialog", &mut agent)
        .expect("apply saved agent memory");

    let task = agent.task_state().expect("task state");
    assert_eq!(task.title, "name pizzeria");
    assert!(
        task.results.iter().any(|item| item.contains("ВолгаПицца")),
        "dialog B should see approved decision from dialog A when both use the same feature task"
    );
}

#[test]
fn saved_agent_restores_formal_task_state_for_any_dialog_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("sessions");
    let store = LocalSessionStore::from_root(root.clone());
    store
        .save_agent(&web_test_agent("app-agent"))
        .expect("save agent");
    store
        .save_dialog_task(
            "app-agent",
            "planning-dialog",
            &TaskContext {
                stage: TaskStage::Execution,
                current_step: "create application shell".to_string(),
                expected_action: "agent_work".to_string(),
                paused: true,
                resume_hint: "name and implementation plan approved".to_string(),
                title: "create application".to_string(),
                goal: "build an approved app".to_string(),
                plan_approved: true,
                plan: vec![
                    "collect app name".to_string(),
                    "approve plan".to_string(),
                    "create application shell".to_string(),
                    "validate result".to_string(),
                ],
                artifacts: vec![TaskArtifact {
                    stage: TaskStage::Planning,
                    key: "plan".to_string(),
                    value: "approved application plan".to_string(),
                }],
                ..TaskContext::default()
            },
        )
        .expect("save shared task");

    let restarted_store = LocalSessionStore::from_root(root);
    let mut agent = web_test_chat_agent();
    apply_saved_for_test(
        &restarted_store,
        Some("app-agent"),
        "any-other-dialog",
        &mut agent,
    )
    .expect("apply saved agent memory");

    let task = agent.task_state().expect("task state");
    assert_eq!(task.stage, TaskStage::Execution);
    assert_eq!(task.current_step, "create application shell");
    assert_eq!(task.expected_action, "agent_work");
    assert!(task.paused);
    assert_eq!(task.resume_hint, "name and implementation plan approved");
    assert_eq!(
        task.plan,
        vec![
            "collect app name",
            "approve plan",
            "create application shell",
            "validate result"
        ]
    );
}

#[test]
fn saved_agent_can_load_different_task_when_dialog_is_assigned_elsewhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("menu-agent"))
        .expect("save agent");
    store
        .save_dialog_task(
            "menu-agent",
            "naming-dialog",
            &TaskContext {
                title: "name pizzeria".to_string(),
                goal: "invent pizzeria name".to_string(),
                results: vec!["Approved decision: ВолгаПицца".to_string()],
                ..TaskContext::default()
            },
        )
        .expect("save default task");
    store
        .assign_dialog_task("menu-agent", "menu-dialog", "menu")
        .expect("assign menu task");
    store
        .save_dialog_task(
            "menu-agent",
            "menu-dialog",
            &TaskContext {
                title: "product menu".to_string(),
                goal: "invent pizza menu".to_string(),
                ..TaskContext::default()
            },
        )
        .expect("save menu task");

    let mut agent = web_test_chat_agent();
    apply_saved_for_test(&store, Some("menu-agent"), "menu-dialog", &mut agent)
        .expect("apply saved agent memory");

    let task = agent.task_state().expect("task state");
    assert_eq!(task.title, "product menu");
    assert_eq!(task.goal, "invent pizza menu");
    assert!(
        task.results.iter().all(|item| !item.contains("ВолгаПицца")),
        "dialogs assigned to another task id should not receive unrelated task results"
    );
}

#[tokio::test]
async fn agents_manage_can_manually_save_and_bind_dialog_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("menu-agent"))
        .expect("save agent");
    let state = web_test_state(store.clone());

    let result = agents_manage(
        State(state),
        ApiJson(AgentsManageRequest {
            action: "task-save".to_string(),
            id: Some("menu-agent".to_string()),
            session_id: Some("menu-dialog".to_string()),
            task_id: Some("menu".to_string()),
            task: Some(TaskPayload {
                stage: "clarify".to_string(),
                title: "menu backlog".to_string(),
                goal: "build potato-first delivery menu".to_string(),
                plan: vec!["draft sections".to_string(), "price combos".to_string()],
                pipeline: vec![
                    TaskPipelineStage {
                        stage: TaskStage::Planning,
                        name: "Research".to_string(),
                        system_prompt: "Research menu constraints.".to_string(),
                        artifact_key: "menu_research".to_string(),
                        worker_agents: vec![TaskWorkerAgent {
                            id: "pricing".to_string(),
                            direction: "pricing".to_string(),
                            system_prompt: "Analyze combo pricing.".to_string(),
                        }],
                        ..TaskPipelineStage::default()
                    },
                    TaskPipelineStage {
                        stage: TaskStage::Execution,
                        name: "Build".to_string(),
                        artifact_key: "menu_draft".to_string(),
                        ..TaskPipelineStage::default()
                    },
                    TaskPipelineStage {
                        stage: TaskStage::Validation,
                        name: "Validate".to_string(),
                        artifact_key: "menu_validation".to_string(),
                        ..TaskPipelineStage::default()
                    },
                ],
                notes: "manual note from owner".to_string(),
            }),
            agent: None,
            token: None,
            token_provider: None,
            title: None,
            swarm: None,
        }),
    )
    .await;
    let Json(response) = match result {
        Ok(response) => response,
        Err(_) => panic!("task-save should succeed"),
    };

    assert_eq!(response.task_id, "menu");
    assert_eq!(response.task.expect("task").stage.to_string(), "clarify");
    assert_eq!(
        store.dialog_task_id("menu-agent", "menu-dialog").unwrap(),
        "menu"
    );

    let mut agent = web_test_chat_agent();
    apply_saved_for_test(&store, Some("menu-agent"), "menu-dialog", &mut agent)
        .expect("apply task");
    let task = agent.task_state().expect("task state");
    assert_eq!(task.goal, "build potato-first delivery menu");
    assert_eq!(task.notes, "manual note from owner");
    assert_eq!(task.pipeline.len(), 3);
    assert_eq!(task.pipeline[0].system_prompt, "Research menu constraints.");
    assert_eq!(task.pipeline[0].worker_agents[0].direction, "pricing");
    assert!(task.artifacts.is_empty());
}

#[tokio::test]
async fn agents_manage_rejects_manual_task_stage_jump() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("guarded-agent"))
        .expect("save agent");
    let state = web_test_state(store);

    let result = agents_manage(
        State(state),
        ApiJson(AgentsManageRequest {
            action: "task-save".to_string(),
            id: Some("guarded-agent".to_string()),
            task_id: Some("default".to_string()),
            task: Some(TaskPayload {
                stage: "done".to_string(),
                title: "unsafe jump".to_string(),
                goal: "skip lifecycle".to_string(),
                plan: Vec::new(),
                pipeline: Vec::new(),
                notes: String::new(),
            }),
            session_id: None,
            agent: None,
            token: None,
            token_provider: None,
            title: None,
            swarm: None,
        }),
    )
    .await;

    let error = result.expect_err("manual stage overwrite must be rejected");
    assert!(error.0.to_string().contains("lifecycle orchestrator"));
}

#[tokio::test]
async fn stateful_postprocess_persists_task_to_shared_feature_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("menu-agent"))
        .expect("save agent");

    let mut agent = web_test_chat_agent();
    agent.set_task_state(Some(TaskContext::default()));
    // The swarm orchestrator runs the turn (seeds goal, sets stage); the answer
    // has no STAGE_DONE marker, so the task stays in the Clarify stage.
    let client = StatefulFakeClient::with_replies(vec!["Конечно, давай уточним тон."]);
    agent
        .respond(&client, "Помоги придумать слоган для пиццерии".to_string())
        .await
        .expect("respond");

    let (debug, _metrics) = run_and_persist_stateful(
        &store,
        Some("menu-agent"),
        "slogan-dialog",
        &mut agent,
        &client,
        "Помоги придумать слоган для пиццерии",
        "Конечно, давай уточним тон.",
    )
    .await
    .expect("persist stateful");

    assert_eq!(debug.expect("debug").stage.as_deref(), Some("clarify"));
    assert_eq!(
        store
            .load_dialog_task("menu-agent", "slogan-dialog")
            .expect("dialog task")
            .goal,
        "Помоги придумать слоган для пиццерии"
    );
    assert!(
        !store
            .load_dialog_task("menu-agent", "menu-dialog")
            .expect("other dialog task")
            .is_empty(),
        "default task should be visible to another dialog in the same feature"
    );
    assert_eq!(
        store
            .load_dialog_task("menu-agent", "menu-dialog")
            .unwrap()
            .goal,
        "Помоги придумать слоган для пиццерии"
    );
    assert!(
        store
            .load_task("menu-agent")
            .expect("legacy/global task")
            .is_empty(),
        "stateful postprocess must not write into the global agent task"
    );
}

#[tokio::test]
async fn invalid_persisted_task_is_repaired_before_streaming_provider_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("legacy-agent"))
        .expect("save agent");
    store
        .save_dialog_task(
            "legacy-agent",
            "legacy-dialog",
            &TaskContext {
                stage: TaskStage::Execution,
                goal: "legacy task without approved plan".to_string(),
                ..TaskContext::default()
            },
        )
        .expect("save legacy task");

    let client = StatefulFakeClient::default();
    let mut agent = web_test_chat_agent();
    apply_saved_for_test(&store, Some("legacy-agent"), "legacy-dialog", &mut agent)
        .expect("load and repair legacy task");

    let repaired = agent.task_state().expect("repaired task");
    assert_eq!(repaired.stage, TaskStage::Planning);
    assert!(
        repaired
            .transition_block_reason
            .contains("без утверждённого")
    );
    let persisted = store
        .load_dialog_task("legacy-agent", "legacy-dialog")
        .expect("load persisted repair");
    assert_eq!(persisted.stage, TaskStage::Planning);

    let prepared = agent
        .prepare_stream_request(&client, "Можно набросать сам текст?")
        .await
        .expect("reject invalid skip");
    assert!(prepared.request.is_none());
    assert!(
        prepared
            .local_response
            .expect("local rejection")
            .text
            .contains('⛔')
    );
    assert_eq!(
        client.call_count(),
        0,
        "repair and lifecycle rejection must happen before any provider call"
    );
}

#[tokio::test]
async fn streaming_local_resume_is_persisted_and_survives_agent_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&web_test_agent("resume-agent"))
        .expect("save agent");
    store
        .save_dialog_task(
            "resume-agent",
            "resume-dialog",
            &TaskContext {
                stage: TaskStage::Execution,
                goal: "implement approved plan".to_string(),
                paused: true,
                pause_reason: TaskPauseReason::Manual,
                plan_approved: true,
                artifacts: vec![TaskArtifact {
                    stage: TaskStage::Planning,
                    key: "plan".to_string(),
                    value: "approved implementation plan".to_string(),
                }],
                ..TaskContext::default()
            },
        )
        .expect("save paused task");

    let client = StatefulFakeClient::default();
    let mut agent = web_test_chat_agent();
    apply_saved_for_test(&store, Some("resume-agent"), "resume-dialog", &mut agent)
        .expect("load paused task");

    let prepared = agent
        .prepare_stream_request(&client, "Продолжай")
        .await
        .expect("prepare local resume");
    let local = prepared.local_response.expect("resume must be local");
    assert!(prepared.request.is_none());
    assert!(!agent.task_state().expect("task").paused);

    let (debug, _metrics) = run_and_persist_stateful(
        &store,
        Some("resume-agent"),
        "resume-dialog",
        &mut agent,
        &client,
        "Продолжай",
        &local.text,
    )
    .await
    .expect("persist resumed task");
    assert!(!debug.expect("agent debug").paused);

    let persisted = store
        .load_dialog_task("resume-agent", "resume-dialog")
        .expect("load persisted task");
    assert_eq!(persisted.stage, TaskStage::Execution);
    assert!(!persisted.paused);
    assert_eq!(persisted.pause_reason, TaskPauseReason::None);

    let mut reloaded = web_test_chat_agent();
    apply_saved_for_test(&store, Some("resume-agent"), "resume-dialog", &mut reloaded)
        .expect("reload resumed task");
    let reloaded_task = reloaded.task_state().expect("reloaded task");
    assert_eq!(reloaded_task.stage, TaskStage::Execution);
    assert!(!reloaded_task.paused);
}

#[test]
fn manually_saved_long_term_fact_uses_saved_agent_scope_across_dialogs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let provider_model_key = "web:local-session-agent:deepseek:deepseek-chat";
    let agent_key = effective_session_key(Some("menu-agent"), provider_model_key);

    let mut first_dialog_memory = AgentMemory::default();
    first_dialog_memory.set_fact_in_layer(
        "available_products".to_string(),
        "tomatoes, basil, mozzarella".to_string(),
        MemoryLayer::LongTerm,
    );
    first_dialog_memory.set_fact_in_layer(
        "current_task".to_string(),
        "invent menu".to_string(),
        MemoryLayer::Working,
    );
    store
        .save_long_term(&agent_key, &first_dialog_memory)
        .expect("save long-term");

    let mut second_dialog_memory = AgentMemory::default();
    store
        .seed_long_term(&agent_key, &mut second_dialog_memory)
        .expect("seed second dialog");

    assert_eq!(
        second_dialog_memory
            .facts
            .get("available_products")
            .map(String::as_str),
        Some("tomatoes, basil, mozzarella")
    );
    assert!(
        !second_dialog_memory.facts.contains_key("current_task"),
        "manual long-term sharing must not bring current working task along"
    );
    assert!(
        store
            .load_long_term(provider_model_key)
            .expect("provider/model memory")
            .facts
            .is_empty(),
        "saved-agent manual memory must not be written under provider/model key"
    );
}

#[test]
fn agent_initial_context_is_freeform_long_term_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let context = "Пиццерия в Волгограде для молодёжи. Название ВолгаПицца утверждено.";

    persist_agent_initial_context(&store, "menu-agent", Some(context))
        .expect("persist initial context");

    let mut second_dialog_memory = AgentMemory::default();
    store
        .seed_long_term("agent:menu-agent", &mut second_dialog_memory)
        .expect("seed long-term");

    assert_eq!(
        second_dialog_memory
            .facts
            .get("project_context")
            .map(String::as_str),
        Some(context)
    );
    assert_eq!(
        second_dialog_memory.fact_layer("project_context"),
        MemoryLayer::LongTerm
    );
}

#[test]
fn structured_initial_context_becomes_separate_profile_and_long_term_facts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let context = "jurisdiction = Россия\nclient_role = ИП, исполнитель\nlegal_area = договорная задолженность";

    persist_agent_initial_context(&store, "legal-agent", Some(context))
        .expect("persist structured context");

    let long_term = store
        .load_long_term("agent:legal-agent")
        .expect("load long-term");
    assert_eq!(
        long_term.facts.get("jurisdiction").map(String::as_str),
        Some("Россия")
    );
    assert_eq!(
        long_term.facts.get("client_role").map(String::as_str),
        Some("ИП, исполнитель")
    );
    assert!(!long_term.facts.contains_key("project_context"));

    let profile = store.load_profile("legal-agent").expect("load profile");
    assert_eq!(profile.fields.len(), 3);
    assert!(profile.fields.iter().all(ProfileField::is_filled));
    assert_eq!(
        profile
            .fields
            .iter()
            .find(|field| field.key == "legal_area")
            .map(|field| field.value.as_str()),
        Some("договорная задолженность")
    );
}

#[test]
fn profile_schema_merge_preserves_known_initial_context_fields() {
    let existing = vec![ProfileField {
        key: "jurisdiction".to_string(),
        question: "jurisdiction".to_string(),
        required: false,
        value: "Россия".to_string(),
    }];
    let merged = merge_profile_schema(
        existing,
        vec![ProfileFieldPayload {
            key: "debtor_type".to_string(),
            question: "debtor_type".to_string(),
            required: true,
        }],
    );

    assert!(merged.iter().any(|field| {
        field.key == "jurisdiction" && field.value == "Россия" && !field.required
    }));
    assert!(merged.iter().any(|field| field.key == "debtor_type"));
}

#[test]
fn agent_initial_context_rejects_sensitive_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));

    let error = persist_agent_initial_context(
        &store,
        "menu-agent",
        Some("openai token sk-1234567890abcdef1234567890abcdef"),
    )
    .expect_err("sensitive context should be rejected");

    assert!(
        error.to_string().contains("looks sensitive"),
        "unexpected error: {error}"
    );
}

#[test]
fn profile_ui_does_not_duplicate_identical_key_and_question() {
    assert!(INDEX_HTML.contains("question !== (f.key || '').trim()"));
    assert!(INDEX_HTML.contains(
        ".map((question) => `<div class=\"profile-field missing\"><span class=\"pf-key\">${escapeHtml(question)}</span></div>`)"
    ));
}

#[test]
fn web_memory_config_maps_strategy_and_limits() {
    let config = WebMemoryConfig {
        strategy: Some("sticky-facts".to_string()),
        recent_messages: Some(4),
        summarize_after_messages: None,
        summary_chunk_messages: None,
        summarize_at_context_percent: None,
        summary_prompt: None,
        facts_extraction_prompt: Some("Collect only favorite colors.".to_string()),
        facts_prompt: Some("Custom provider facts prompt".to_string()),
        active_branch: Some("alpha".to_string()),
        scoped_auto_route: Some(false),
        topic_file_routing: None,
        topic_drift_guard: None,
        topic_auto_create: None,
        topic_classifier_prompt: None,
    }
    .into_memory_config();

    assert_eq!(config.strategy, MemoryStrategy::StickyFacts);
    assert_eq!(config.recent_messages, 4);
    assert_eq!(
        config.facts_extraction_prompt,
        "Collect only favorite colors."
    );
    assert_eq!(config.facts_prompt, "Custom provider facts prompt");
    assert_eq!(config.active_branch, "alpha");
    assert!(!config.scoped_auto_route);
}

#[test]
fn web_memory_config_supports_all_context_strategies() {
    let cases = [
        ("summary", MemoryStrategy::Summary),
        ("sliding-window", MemoryStrategy::SlidingWindow),
        ("sticky-facts", MemoryStrategy::StickyFacts),
        ("branching", MemoryStrategy::Branching),
        ("scoped-branches", MemoryStrategy::ScopedBranches),
        ("unknown", MemoryStrategy::StickyFacts),
    ];

    for (strategy, expected) in cases {
        let config = WebMemoryConfig {
            strategy: Some(strategy.to_string()),
            recent_messages: Some(7),
            summarize_after_messages: None,
            summary_chunk_messages: None,
            summarize_at_context_percent: None,
            summary_prompt: None,
            facts_extraction_prompt: None,
            facts_prompt: None,
            active_branch: None,
            scoped_auto_route: None,
            topic_file_routing: None,
            topic_drift_guard: None,
            topic_auto_create: None,
            topic_classifier_prompt: None,
        }
        .into_memory_config();

        assert_eq!(config.strategy, expected, "strategy={strategy}");
        assert_eq!(config.recent_messages, 7);
    }
}

#[test]
fn web_memory_config_blank_facts_prompt_uses_default() {
    let config = WebMemoryConfig {
        strategy: Some("sticky-facts".to_string()),
        recent_messages: Some(4),
        summarize_after_messages: None,
        summary_chunk_messages: None,
        summarize_at_context_percent: None,
        summary_prompt: Some("   ".to_string()),
        facts_extraction_prompt: Some("   ".to_string()),
        facts_prompt: Some("   ".to_string()),
        active_branch: Some("   ".to_string()),
        scoped_auto_route: None,
        topic_file_routing: None,
        topic_drift_guard: None,
        topic_auto_create: None,
        topic_classifier_prompt: Some("   ".to_string()),
    }
    .into_memory_config();

    assert_eq!(
        config.summary_prompt,
        crate::chat::memory::DEFAULT_SUMMARY_PROMPT
    );
    assert_eq!(
        config.facts_extraction_prompt,
        crate::chat::memory::DEFAULT_FACTS_EXTRACTION_PROMPT
    );
    assert_eq!(
        config.facts_prompt,
        crate::chat::memory::DEFAULT_FACTS_PROMPT
    );
    assert_eq!(config.active_branch, "default");
}

#[test]
fn web_memory_config_maps_summary_settings_and_prompt() {
    let config = WebMemoryConfig {
        strategy: Some("summary".to_string()),
        recent_messages: Some(6),
        summarize_after_messages: Some(11),
        summary_chunk_messages: Some(5),
        summarize_at_context_percent: Some(72),
        summary_prompt: Some("Keep only durable project memory.".to_string()),
        facts_extraction_prompt: None,
        facts_prompt: None,
        active_branch: None,
        scoped_auto_route: None,
        topic_file_routing: None,
        topic_drift_guard: None,
        topic_auto_create: None,
        topic_classifier_prompt: None,
    }
    .into_memory_config();

    assert_eq!(config.strategy, MemoryStrategy::Summary);
    assert_eq!(config.recent_messages, 6);
    assert_eq!(config.summarize_after_messages, 11);
    assert_eq!(config.summary_chunk_messages, 5);
    assert_eq!(config.summarize_at_context_percent, 72);
    assert_eq!(config.summary_prompt, "Keep only durable project memory.");
}

#[test]
fn web_context_debug_reports_scoped_topics_and_active_topic() {
    let history = vec![
        ChatMessage {
            role: Role::User,
            content: "Rust async".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "Rust answer".to_string(),
        },
        ChatMessage {
            role: Role::User,
            content: "Vacation budget".to_string(),
        },
    ];
    let mut memory = crate::chat::AgentMemory::default();
    memory
        .branch_assignments
        .insert("0".to_string(), "rust".to_string());
    memory
        .branch_assignments
        .insert("1".to_string(), "rust".to_string());
    memory
        .branch_assignments
        .insert("2".to_string(), "travel".to_string());
    let config = MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        active_branch: "travel".to_string(),
        scoped_auto_route: true,
        ..MemoryConfig::default()
    };

    let debug = build_context_debug(&memory, &config, &history);

    assert_eq!(debug.strategy, "scoped-branches");
    assert_eq!(debug.active_topic, "travel");
    assert!(debug.scoped_auto_route);
    assert!(
        debug
            .scoped_topics
            .iter()
            .any(|topic| topic.name == "rust" && topic.message_count == 2 && !topic.active)
    );
    assert!(
        debug
            .scoped_topics
            .iter()
            .any(|topic| topic.name == "travel" && topic.message_count == 1 && topic.active)
    );
}

#[test]
fn web_context_debug_reports_persisted_facts_and_request_block() {
    let history = vec![
        ChatMessage {
            role: Role::User,
            content: "goal: ship KV facts".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "ok".to_string(),
        },
        ChatMessage {
            role: Role::User,
            content: "recent question".to_string(),
        },
    ];
    let mut memory = crate::chat::AgentMemory::default();
    memory
        .facts
        .insert("goal".to_string(), "ship KV facts".to_string());
    memory
        .facts
        .insert("constraints".to_string(), "show debug".to_string());
    let config = MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 2,
        facts_prompt: "Facts are read-only.".to_string(),
        ..MemoryConfig::default()
    };

    let debug = build_context_debug(&memory, &config, &history);

    assert_eq!(debug.strategy, "sticky-facts");
    assert_eq!(debug.facts.persisted.len(), 2);
    assert!(
        debug
            .facts
            .persisted
            .iter()
            .any(|fact| fact.key == "goal" && fact.value == "ship KV facts")
    );
    let request_block = debug.facts.request_block.expect("facts request block");
    assert_eq!(
        debug.facts.extraction_prompt.as_deref(),
        Some(crate::chat::memory::DEFAULT_FACTS_EXTRACTION_PROMPT)
    );
    assert!(request_block.starts_with("Facts are read-only."));
    assert!(request_block.contains("FACTS_KV:"));
    assert!(request_block.contains("- goal: ship KV facts"));
    assert!(request_block.contains("- constraints: show debug"));
    assert_eq!(debug.facts.recent_messages_sent, 2);
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
        saved_agent_id: None,
        user_profile_id: None,
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
        saved_agent_id: None,
        user_profile_id: None,
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
        INDEX_HTML.contains("setMetrics(data.metrics, data.context_metrics);"),
        "streaming done handler must render per-request metrics so request cost is not blank"
    );
    assert!(
        INDEX_HTML.contains("context_metrics"),
        "streaming done handler must receive separate context metrics"
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
fn web_ui_stream_abort_labels_partial_response_as_unsaved() {
    assert!(
        INDEX_HTML.contains("Остановлено: частичный ответ не сохранён"),
        "partial stream abort should not look like a fully persisted turn"
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
fn web_ui_displays_context_status_separately() {
    assert!(INDEX_HTML.contains(">Контекст</span>"));
    assert!(INDEX_HTML.contains("id=\"metricSummary\""));
    assert!(INDEX_HTML.contains("function contextLines(metrics)"));
    assert!(
        INDEX_HTML.contains("function strategyContextLabel()"),
        "UI should describe selected context strategy when no extra context metrics exist"
    );
}

#[test]
fn web_ui_context_settings_use_human_labels() {
    assert!(INDEX_HTML.contains(">Summary</option>"));
    assert!(INDEX_HTML.contains("Sliding Window"));
    assert!(INDEX_HTML.contains("Sticky Facts"));
    assert!(INDEX_HTML.contains("Branching"));
    assert!(INDEX_HTML.contains("Scoped Branches"));
    assert!(INDEX_HTML.contains("id=\"memoryRecentTitle\""));
    assert!(INDEX_HTML.contains("id=\"memoryRecentHelp\""));
    assert!(INDEX_HTML.contains("Raw сообщений рядом с summary"));
    assert!(INDEX_HTML.contains("Хранить последние N сообщений"));
    assert!(INDEX_HTML.contains("Свежие сообщения вместе с facts"));
    assert!(INDEX_HTML.contains("Сообщения текущей ветки"));
    assert!(INDEX_HTML.contains("Сообщения выбранной темы"));
    assert!(INDEX_HTML.contains("Auto topics в одном чате"));
    assert!(INDEX_HTML.contains("id=\"memoryScopedAutoRoute\""));
    assert!(INDEX_HTML.contains("id=\"scopedTopicDebug\""));
    assert!(INDEX_HTML.contains("id=\"contextDebugPanel\""));
    assert!(INDEX_HTML.contains("Summary prompt"));
    assert!(INDEX_HTML.contains("id=\"memorySummaryPrompt\""));
    assert!(INDEX_HTML.contains("id=\"memorySummarizeAfterMessages\""));
    assert!(INDEX_HTML.contains("id=\"memorySummaryChunkMessages\""));
    assert!(INDEX_HTML.contains("id=\"memorySummarizeAtContextPercent\""));
    assert!(INDEX_HTML.contains("Facts extraction prompt"));
    assert!(INDEX_HTML.contains("id=\"memoryFactsExtractionPrompt\""));
    assert!(INDEX_HTML.contains("Facts preamble"));
    assert!(INDEX_HTML.contains("id=\"memoryFactsPrompt\""));
    assert!(INDEX_HTML.contains("id=\"factsKvPreview\""));
    assert!(INDEX_HTML.contains("Provider facts block"));
    assert!(INDEX_HTML.contains("EXTRACTION_PROMPT:"));
    assert!(INDEX_HTML.contains("id=\"memoryActiveBranch\""));
    assert!(
        !INDEX_HTML.contains(">memory_strategy<"),
        "context settings should not expose raw API field names as labels"
    );
}

#[test]
fn web_ui_branching_controls_are_visible_and_simple() {
    assert!(INDEX_HTML.contains("id=\"checkpointBranches\""));
    assert!(INDEX_HTML.contains("id=\"branchA\""));
    assert!(INDEX_HTML.contains("id=\"branchB\""));
    assert!(INDEX_HTML.contains("function createBranchCheckpoint()"));
    assert!(INDEX_HTML.contains("function switchBranch(id)"));
}

#[test]
fn web_ui_branch_payload_uses_active_branch_session_and_messages() {
    let marker = "function chatPayload()";
    let start = INDEX_HTML.find(marker).expect("chatPayload");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 1_900)];

    assert!(
        body.contains("session_id: currentBranch()?.sessionId || chatSessionId"),
        "branching requests must use active branch session id"
    );
    assert!(
        body.contains(
            "new_session: currentBranch() ? !currentBranch().sessionId : forceNewSession"
        ),
        "new branch without session id must create an independent backend session"
    );
    assert!(
        body.contains("messages: currentBranch()?.messages || chatHistory"),
        "branching requests must send active branch history, not global chat history"
    );
}

#[test]
fn web_ui_memory_payload_includes_custom_facts_prompt() {
    let marker = "function memoryPayload()";
    let start = INDEX_HTML.find(marker).expect("memoryPayload");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 900)];

    assert!(
        body.contains("summary_prompt: textValue('memorySummaryPrompt')"),
        "custom summary prompt must be sent to the web API"
    );
    assert!(
        body.contains("summarize_after_messages: numberValue('memorySummarizeAfterMessages')"),
        "summary after threshold must be sent to the web API"
    );
    assert!(
        body.contains("summary_chunk_messages: numberValue('memorySummaryChunkMessages')"),
        "summary chunk size must be sent to the web API"
    );
    assert!(
        body.contains(
            "summarize_at_context_percent: numberValue('memorySummarizeAtContextPercent')"
        ),
        "summary context-pressure percent must be sent to the web API"
    );
    assert!(
        body.contains("facts_extraction_prompt: textValue('memoryFactsExtractionPrompt')"),
        "custom facts extraction prompt must be sent to the web API"
    );
    assert!(
        body.contains("facts_prompt: textValue('memoryFactsPrompt')"),
        "custom facts prompt must be sent to the web API"
    );
    assert!(
        body.contains("active_branch: textValue('memoryActiveBranch') || 'default'"),
        "manual internal topic must be sent to the web API"
    );
    assert!(
        body.contains("scoped_auto_route: $('memoryScopedAutoRoute').checked"),
        "auto topic routing flag must be sent to the web API"
    );
}

#[test]
fn web_ui_renders_context_topic_debug() {
    assert!(INDEX_HTML.contains("function setContextDebug(contextDebug)"));
    assert!(INDEX_HTML.contains("contextDebug.scoped_topics"));
    assert!(INDEX_HTML.contains("Активная тема:"));
    assert!(INDEX_HTML.contains("topic-chip"));
    assert!(INDEX_HTML.contains("setContextDebug(data.context_debug);"));
}

#[test]
fn web_ui_facts_preview_detects_custom_prompt_facts_block() {
    let marker = "function updateFactsPreview(debug)";
    let start = INDEX_HTML.find(marker).expect("updateFactsPreview");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 700)];

    assert!(
        body.contains("includes('FACTS_KV:')"),
        "facts preview should detect the explicit provider facts block marker"
    );
}

#[test]
fn web_ui_is_agent_centric() {
    // Agent gate is the entry screen; workspace is gated behind an active agent.
    assert!(INDEX_HTML.contains("id=\"agentGate\""));
    assert!(INDEX_HTML.contains("id=\"workspace\""));
    assert!(INDEX_HTML.contains("id=\"activeAgentBar\""));
    assert!(INDEX_HTML.contains("data-tab=\"userprofile\""));
    assert!(INDEX_HTML.contains("id=\"userProfileSelect\""));
    assert!(INDEX_HTML.contains("id=\"userProfileInstructions\""));
    assert!(INDEX_HTML.contains("Как отвечать пользователю"));
    assert!(INDEX_HTML.contains("custom_instructions: $('userProfileInstructions').value.trim()"));
    assert!(!INDEX_HTML.contains("id=\"userProfileStyle\""));
    assert!(!INDEX_HTML.contains("id=\"userProfileFormat\""));
    assert!(!INDEX_HTML.contains("id=\"userProfileConstraints\""));
    assert!(!INDEX_HTML.contains("id=\"userProfileLanguage\""));
    assert!(!INDEX_HTML.contains("id=\"userProfileCustom\""));
    assert!(!INDEX_HTML.contains("по одному правилу на строку"));
    assert!(INDEX_HTML.contains("/api/user-profiles/manage"));
    assert!(INDEX_HTML.contains("user_profile_id: activeUserProfileId || '__none__'"));
    assert!(INDEX_HTML.contains("Переключение не меняет agent definition"));
    assert!(INDEX_HTML.contains("Создать агента"));
    assert!(INDEX_HTML.contains("id=\"newAgent\""));
    assert!(INDEX_HTML.contains("function showCreateAgentGate()"));
    assert!(INDEX_HTML.contains("$('newAgent').addEventListener('click', showCreateAgentGate);"));
    assert!(INDEX_HTML.contains("function resetGateCreateForm()"));
    assert!(INDEX_HTML.contains("Сгенерировать интервью"));
    assert!(INDEX_HTML.contains("id=\"gateModel\""));
    assert!(INDEX_HTML.contains("id=\"gateLoadModels\""));
    assert!(INDEX_HTML.contains("function gateLoadModels()"));
    assert!(INDEX_HTML.contains("function gateProviderPayload()"));
    assert!(INDEX_HTML.contains("id=\"gateCustomModel\""));
    assert!(INDEX_HTML.contains("Информация о проекте и агенте"));
    assert!(INDEX_HTML.contains("id=\"gateInitialContext\""));
    assert!(INDEX_HTML.contains("Стартовые факты вкладки Профиль"));
    assert!(INDEX_HTML.contains("initial_context: $('gateInitialContext').value.trim()"));
    assert!(INDEX_HTML.contains("Что агент должен уточнить"));
    assert!(INDEX_HTML.contains("domain: $('gateDomain').value.trim()"));
    assert!(!INDEX_HTML.contains("Обязательные поля профиля (по одному на строку"));
    // Stateful panes: profile (interview), task (stage FSM), invariants.
    assert!(INDEX_HTML.contains("data-tab=\"aprofile\""));
    assert!(INDEX_HTML.contains("id=\"profileAgentContext\""));
    assert!(INDEX_HTML.contains("id=\"profileAgentDomain\""));
    assert!(INDEX_HTML.contains("id=\"profileAgentInvariants\""));
    assert!(INDEX_HTML.contains("id=\"profileAgentContextSave\""));
    assert!(INDEX_HTML.contains("function renderProfileAgentContext(agent)"));
    assert!(INDEX_HTML.contains("function saveAgentContextFromProfile()"));
    assert!(INDEX_HTML.contains("renderProfileAgentContext(agent);"));
    assert!(INDEX_HTML.contains("renderProfileAgentContext(data.agent);"));
    assert!(INDEX_HTML.contains("[agent:domain]"));
    assert!(INDEX_HTML.contains("id=\"profileLongTermFacts\""));
    assert!(INDEX_HTML.contains("id=\"profileLongTermAdd\""));
    assert!(INDEX_HTML.contains("function renderProfileLongTermFacts(facts)"));
    assert!(INDEX_HTML.contains("layer: 'long-term'"));
    assert!(INDEX_HTML.contains("data-tab=\"task\""));
    assert!(INDEX_HTML.contains("id=\"taskCurrentStepView\""));
    assert!(INDEX_HTML.contains("id=\"taskRequirementView\""));
    assert!(INDEX_HTML.contains("id=\"taskNextStageView\""));
    assert!(INDEX_HTML.contains("id=\"taskBlockView\""));
    assert!(INDEX_HTML.contains("id=\"taskResumeHintView\""));
    assert!(INDEX_HTML.contains("id=\"taskGoalView\""));
    assert!(INDEX_HTML.contains("Будущий stage-agent не запустится"));
    assert!(!INDEX_HTML.contains("id=\"taskIdInput\""));
    assert!(!INDEX_HTML.contains("id=\"taskLoad\""));
    assert!(!INDEX_HTML.contains("id=\"taskSave\""));
    assert!(!INDEX_HTML.contains("id=\"taskStageSelect\""));
    assert!(!INDEX_HTML.contains("function saveTaskFromForm()"));
    assert!(INDEX_HTML.contains("data-tab=\"invariants\""));
    assert!(INDEX_HTML.contains("Стадию меняет только оркестратор"));
    assert!(INDEX_HTML.contains("id=\"swarmActivity\""));
    assert!(INDEX_HTML.contains("function renderSwarmActivity(report)"));
    assert!(!INDEX_HTML.contains("общая для всех диалогов агента"));
    // Wiring: build-schema action, gate↔workspace, agent_state rendering.
    assert!(INDEX_HTML.contains("/api/agents/manage"));
    assert!(INDEX_HTML.contains("'build-schema'"));
    assert!(INDEX_HTML.contains("function showGate()"));
    assert!(INDEX_HTML.contains("function renderAgentState(state)"));
    assert!(INDEX_HTML.contains("saved_agent_id: activeAgentId"));
    assert!(INDEX_HTML.contains("always show the chooser/create gate on load"));
    assert!(!INDEX_HTML.contains("if (exists) { await enterAgent(activeAgentId); }"));
    assert!(!INDEX_HTML.contains("id=\"agentMode\""));
    assert!(INDEX_HTML.contains("function backendAgentId()"));
    // Multiple dialogs per agent: chat list + switch/new/delete.
    assert!(INDEX_HTML.contains("id=\"dialogsBar\""));
    assert!(INDEX_HTML.contains("'dialogs'"));
    assert!(INDEX_HTML.contains("function switchDialog(sessionId)"));
    assert!(INDEX_HTML.contains("'dialog-delete'"));
}

#[test]
fn web_ui_renders_memory_layers_control() {
    // 3-layer debug panel + manual control wired to the endpoint.
    assert!(INDEX_HTML.contains("Модель памяти — слои"));
    assert!(INDEX_HTML.contains("function renderMemoryLayers(layers)"));
    assert!(INDEX_HTML.contains("renderMemoryLayers(contextDebug.layers)"));
    assert!(INDEX_HTML.contains("🟡 Краткосрочная"));
    assert!(INDEX_HTML.contains("🔵 Рабочая"));
    assert!(INDEX_HTML.contains("🟢 Долговременная"));
    assert!(INDEX_HTML.contains("/api/memory/update"));
    assert!(INDEX_HTML.contains("saved_agent_id: activeAgentId || null"));
    assert!(INDEX_HTML.contains("memoryFactAdd"));
}

#[test]
fn web_ui_clears_previous_swarm_report_when_entering_an_agent() {
    let enter_agent = INDEX_HTML
        .split("async function enterAgent(id)")
        .nth(1)
        .expect("enterAgent function")
        .split("// Reload the agent's stored layers")
        .next()
        .expect("enterAgent body");
    assert!(enter_agent.contains("renderSwarmActivity(null);"));
}

#[test]
fn web_ui_profile_tab_long_term_editor_is_actionable() {
    let marker = "$('profileLongTermAdd').addEventListener('click'";
    let start = INDEX_HTML.find(marker).expect("profile long-term handler");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 900)];

    assert!(
        body.contains(
            "memoryUpdate('set', { layer: 'long-term', key, value }, 'profileLongTermStatus')"
        ),
        "profile tab must write facts into the long-term layer, not only render static text"
    );
    assert!(body.contains("$('profileLongTermKey').value = '';"));
    assert!(body.contains("$('profileLongTermValue').value = '';"));
    assert!(body.contains("Укажите ключ и значение."));
}

#[test]
fn web_ui_profile_tab_can_edit_agent_context_without_erasing_domain() {
    assert!(INDEX_HTML.contains("domain: $('profileAgentDomain').value.trim()"));
    assert!(INDEX_HTML.contains("invariants: lineToList('profileAgentInvariants')"));
    assert!(INDEX_HTML.contains(
        "$('profileAgentContextSave').addEventListener('click', saveAgentContextFromProfile);"
    ));

    let marker = "async function saveInvariants()";
    let start = INDEX_HTML.find(marker).expect("saveInvariants");
    let mut end = INDEX_HTML.len().min(start + 900);
    while !INDEX_HTML.is_char_boundary(end) {
        end -= 1;
    }
    let body = &INDEX_HTML[start..end];
    assert!(
        body.contains("agent: agentContextPayload()"),
        "saving invariants must preserve the editable agent domain/context"
    );
    assert!(
        !body.contains("domain: null"),
        "saving invariants must not wipe [agent:domain]"
    );
}

#[test]
fn web_ui_empty_user_profile_selection_disables_runtime_profile() {
    let marker = "function chatPayload()";
    let start = INDEX_HTML.find(marker).expect("chatPayload");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 1600)];

    assert!(
        body.contains("user_profile_id: activeUserProfileId || '__none__'"),
        "empty UI selection must send an explicit no-profile sentinel"
    );
}

#[test]
fn web_ui_renders_persisted_facts_and_provider_block() {
    assert!(INDEX_HTML.contains("function renderFactsDebug(factsDebug)"));
    assert!(INDEX_HTML.contains("factsDebug?.persisted"));
    assert!(INDEX_HTML.contains("factsDebug.request_block"));
    assert!(INDEX_HTML.contains("RECENT_MESSAGES_SENT"));
    assert!(INDEX_HTML.contains("renderFactsDebug(contextDebug.facts);"));
}

#[test]
fn web_ui_checkpoint_clones_same_history_into_two_independent_branches() {
    let marker = "function createBranchCheckpoint()";
    let start = INDEX_HTML.find(marker).expect("createBranchCheckpoint");
    let body = &INDEX_HTML[start..INDEX_HTML.len().min(start + 1_200)];

    assert!(body.contains("const checkpoint = chatHistory.slice();"));
    assert!(body.contains("id: 'branch-a'"));
    assert!(body.contains("id: 'branch-b'"));
    assert_eq!(
        body.matches("messages: checkpoint.slice()").count(),
        2,
        "both branches must get separate message arrays from the same checkpoint"
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
        INDEX_HTML.contains("if (needsPricing) modelPricingById.set(model, data.pricing.pricing);"),
        "catalog pricing should not overwrite manual/provider pricing while resolving context"
    );
}

#[test]
fn web_attachments_merge_into_prompt() {
    let prompt = "What is this?";
    let attachments_vec = vec![
        WebAttachment {
            name: "data.txt".to_string(),
            content: "some data".to_string(),
        },
        WebAttachment {
            name: "info.txt".to_string(),
            content: "more info".to_string(),
        },
    ];

    let result = build_web_prompt(prompt, Some(attachments_vec.as_slice()));

    assert!(
        result.contains("What is this?"),
        "original prompt must be preserved"
    );
    assert!(
        result.contains("--- data.txt ---"),
        "attachment name must appear as separator"
    );
    assert!(
        result.contains("some data"),
        "attachment content must be included"
    );
    assert!(
        result.contains("--- info.txt ---\nmore info"),
        "multiple attachments must be separated by name header"
    );
    assert!(
        result.contains("\n\n"),
        "prompt and attachments must be separated by double newline"
    );
}

#[test]
fn web_attachments_empty_are_ignored() {
    let prompt = "What is this?";
    let attachments_vec = vec![
        WebAttachment {
            name: "empty.txt".to_string(),
            content: "".to_string(),
        },
        WebAttachment {
            name: "data.txt".to_string(),
            content: "data".to_string(),
        },
    ];

    let result = build_web_prompt(prompt, Some(attachments_vec.as_slice()));

    assert_eq!(
        result, "What is this?\n\n--- data.txt ---\ndata",
        "empty attachments must be filtered out"
    );
}

#[test]
fn web_attachments_none_returns_original_prompt() {
    let prompt = "Original prompt";
    let result = build_web_prompt(prompt, None);
    assert_eq!(result, prompt, "no attachments = return prompt as-is");
}

#[test]
fn streaming_stays_default_for_deepseek_agents() {
    assert!(INDEX_HTML.contains("id=\"streamMode\" type=\"checkbox\" checked"));
    assert!(!INDEX_HTML.contains("agent.provider !== 'deepseek'"));
    assert!(!INDEX_HTML.contains("localStorage.setItem('ai_stream_mode', 'off')"));
}

#[test]
fn streaming_parser_keeps_sse_state_across_network_chunks() {
    assert!(INDEX_HTML.contains("function parseSseEventBlock(block)"));
    assert!(INDEX_HTML.contains("sseBuffer.indexOf('\\n\\n')"));
    assert!(INDEX_HTML.contains("handleSseEvent(parseSseEventBlock(sseBuffer))"));
    assert!(!INDEX_HTML.contains("let eventType = null;\n          let eventData = null;"));
}

#[test]
fn clearing_request_debug_does_not_clear_saved_profile_facts() {
    let null_branch = INDEX_HTML
        .split("if (!layers) {")
        .nth(1)
        .and_then(|tail| tail.split("return;").next())
        .expect("renderMemoryLayers null branch");
    assert!(!null_branch.contains("renderProfileLongTermFacts(null)"));
}

#[test]
fn mcp_resolve_prefers_url_over_command_and_server() {
    let (label, server) = resolve_mcp_server(McpToolsRequest {
        server: Some("configured".to_string()),
        command: Some("npx".to_string()),
        args: vec!["-y".to_string()],
        url: Some("https://host.test/mcp".to_string()),
    })
    .expect("resolve url");
    assert_eq!(label, "https://host.test/mcp");
    assert!(matches!(server, McpServerConfig::Http { .. }));
}

#[test]
fn mcp_resolve_prefers_command_over_server_and_builds_label() {
    let (label, server) = resolve_mcp_server(McpToolsRequest {
        server: Some("configured".to_string()),
        command: Some("npx".to_string()),
        args: vec!["-y".to_string(), "srv".to_string()],
        url: None,
    })
    .expect("resolve command");
    assert_eq!(label, "npx -y srv");
    match server {
        McpServerConfig::Stdio { command, args } => {
            assert_eq!(command, "npx");
            assert_eq!(args, vec!["-y".to_string(), "srv".to_string()]);
        }
        other => panic!("expected stdio server, got {other:?}"),
    }
}

#[test]
fn mcp_resolve_empty_request_is_invalid_input() {
    let error = resolve_mcp_server(McpToolsRequest {
        server: None,
        command: None,
        args: Vec::new(),
        url: None,
    })
    .expect_err("empty request must be rejected");
    assert!(matches!(error.0, AppError::InvalidInput(_)));
}

#[test]
fn mcp_resolve_treats_blank_fields_as_absent() {
    let error = resolve_mcp_server(McpToolsRequest {
        server: Some("   ".to_string()),
        command: Some("".to_string()),
        args: Vec::new(),
        url: Some("  ".to_string()),
    })
    .expect_err("blank fields must be rejected");
    assert!(matches!(error.0, AppError::InvalidInput(_)));
}

#[test]
fn mcp_tools_request_defaults_missing_fields() {
    let request: McpToolsRequest = serde_json::from_str("{}").expect("deserialize empty body");
    assert!(request.server.is_none());
    assert!(request.command.is_none());
    assert!(request.url.is_none());
    assert!(request.args.is_empty());
}

#[test]
fn mcp_tools_response_omits_error_on_success() {
    let response = McpToolsResponse {
        ok: true,
        server: "fixture".to_string(),
        tool_count: 1,
        tools: vec![McpToolInfo {
            name: "alpha".to_string(),
            description: Some("desc".to_string()),
        }],
        error: None,
    };
    let value = serde_json::to_value(&response).expect("serialize");
    assert_eq!(value["ok"], true);
    assert_eq!(value["tool_count"], 1);
    assert_eq!(value["tools"][0]["name"], "alpha");
    assert!(value.get("error").is_none());
}

#[test]
fn mcp_tools_response_includes_error_on_failure() {
    let response = McpToolsResponse {
        ok: false,
        server: "fixture".to_string(),
        tool_count: 0,
        tools: Vec::new(),
        error: Some("could not connect".to_string()),
    };
    let value = serde_json::to_value(&response).expect("serialize");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "could not connect");
}

#[test]
fn mcp_build_saved_server_prefers_url() {
    let server = build_saved_server(&McpServerSaveRequest {
        name: "x".to_string(),
        command: "npx".to_string(),
        args: vec!["-y".to_string()],
        url: "https://host.test/mcp".to_string(),
        delete: false,
    })
    .expect("build http");
    assert!(matches!(server, McpServerConfig::Http { url } if url == "https://host.test/mcp"));
}

#[test]
fn mcp_build_saved_server_uses_command_when_no_url() {
    let server = build_saved_server(&McpServerSaveRequest {
        name: "x".to_string(),
        command: "npx".to_string(),
        args: vec!["-y".to_string(), "srv".to_string()],
        url: "  ".to_string(),
        delete: false,
    })
    .expect("build stdio");
    match server {
        McpServerConfig::Stdio { command, args } => {
            assert_eq!(command, "npx");
            assert_eq!(args, vec!["-y".to_string(), "srv".to_string()]);
        }
        other => panic!("expected stdio, got {other:?}"),
    }
}

#[test]
fn mcp_build_saved_server_requires_command_or_url() {
    let error = build_saved_server(&McpServerSaveRequest {
        name: "x".to_string(),
        command: String::new(),
        args: Vec::new(),
        url: String::new(),
        delete: false,
    })
    .expect_err("must reject empty");
    assert!(matches!(error.0, AppError::InvalidInput(_)));
}
