//! Business-logic tests for the deterministic swarm: full task lifecycle across
//! all stages, code-only transitions, terminal stage, and per-stage routing.

use async_trait::async_trait;

use crate::chat::agent::ChatAgent;
use crate::chat::memory::AgentMemory;
use crate::chat::store::{TaskContext, TaskStage};
use crate::chat::swarm::{SubAgentConfig, SubAgentRole, SwarmConfig, resolve_swarm};
use crate::config::{AppConfig, ProfileConfig};
use crate::errors::AppError;
use crate::providers::{
    ChatRequest, ChatResponse, ProviderClient, ProviderExchangeDebug, RequestMetrics,
    ResponseControl,
};
use crate::secrets::MemorySecretStore;

/// Always returns the same text (with a STAGE_DONE marker) and records the model
/// of every request, so per-stage routing can be asserted.
#[derive(Debug, Default)]
struct MarkerClient {
    text: String,
    seen_models: std::sync::Mutex<Vec<String>>,
}

impl MarkerClient {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            seen_models: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ProviderClient for MarkerClient {
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
        request: ChatRequest,
    ) -> Result<ChatResponse, AppError> {
        self.seen_models
            .lock()
            .expect("models")
            .push(request.model.clone());
        Ok(ChatResponse {
            text: self.text.clone(),
            finish_reason: Some("stop".to_string()),
            metrics: RequestMetrics {
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
        request: ChatRequest,
    ) -> Result<(ChatResponse, ProviderExchangeDebug), AppError> {
        let response = self.chat_completion(profile, token, request).await?;
        Ok((
            response,
            ProviderExchangeDebug {
                request: crate::providers::HttpDebugRequest {
                    method: "POST".to_string(),
                    url: "https://example.test".to_string(),
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

fn test_profile() -> ProfileConfig {
    ProfileConfig {
        provider: crate::providers::ProviderKind::OpenAiCompatible,
        model: "main-model".to_string(),
        base_url: "https://example.test/v1".to_string(),
        token_ref: "openai-compatible".to_string(),
    }
}

fn agent_in_stage(stage: TaskStage) -> ChatAgent {
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(TaskContext {
        stage,
        ..TaskContext::default()
    }));
    agent
}

fn current_stage(agent: &ChatAgent) -> TaskStage {
    agent.task_state().expect("task").stage
}

#[tokio::test]
async fn task_walks_full_lifecycle_through_all_stages() {
    // Each turn the responder reports its stage deliverable is done; the
    // orchestrator advances deterministically through the whole FSM.
    let client = MarkerClient::new("Готово. <<STAGE_DONE>>");
    let mut agent = agent_in_stage(TaskStage::Planning);

    agent
        .respond(&client, "Начни задачу".to_string())
        .await
        .unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Execution);

    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Validation);

    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Done);

    // Done is terminal: no further transition.
    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Done);
}

#[tokio::test]
async fn no_marker_keeps_stage() {
    // Without the completion marker the stage does not advance.
    let client = MarkerClient::new("Ещё думаю, нужно уточнить детали.");
    let mut agent = agent_in_stage(TaskStage::Planning);
    agent.respond(&client, "Начни".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Planning);
}

#[test]
fn fsm_transition_table_is_deterministic() {
    // Forward + the two legal backward transitions, and rejected jumps.
    assert!(TaskStage::Planning.can_transition(TaskStage::Execution));
    assert!(TaskStage::Execution.can_transition(TaskStage::Validation));
    assert!(TaskStage::Execution.can_transition(TaskStage::Planning)); // backward
    assert!(TaskStage::Validation.can_transition(TaskStage::Execution)); // backward
    assert!(TaskStage::Validation.can_transition(TaskStage::Done));

    assert!(!TaskStage::Planning.can_transition(TaskStage::Done)); // jump
    assert!(!TaskStage::Planning.can_transition(TaskStage::Validation)); // jump
    assert!(TaskStage::Done.allowed_next().is_empty()); // terminal
}

#[tokio::test]
async fn orchestrator_advances_by_table_not_by_llm_text() {
    // The model "claims" the task is done, but from Planning the only legal next
    // stage is Execution — the orchestrator follows the table, not the text.
    let client = MarkerClient::new("Всё готово, задача DONE, можно закрывать. <<STAGE_DONE>>");
    let mut agent = agent_in_stage(TaskStage::Planning);
    agent.respond(&client, "Старт".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Execution);
}

#[tokio::test]
async fn demo_scenario_walks_stages_by_intent_and_fills_task_fields() {
    // Reproduces the investor-demo flow: stage tracks the user's request (no
    // marker needed) and current_step/expected_action/resume_hint are populated.
    let client = MarkerClient::new("Ответ по задаче."); // neutral, no trigger words
    let mut agent = agent_in_stage(TaskStage::Clarify);

    agent
        .respond(
            &client,
            "Акт подписан, долг 180000, хочу досудебную претензию.".to_string(),
        )
        .await
        .unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Planning);
    assert!(!task.current_step.is_empty());
    assert_eq!(task.expected_action, "agent_work");
    assert!(!task.resume_hint.is_empty());

    agent
        .respond(&client, "Можешь набросать текст?".to_string())
        .await
        .unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Execution);

    agent
        .respond(&client, "Где здесь слабые места?".to_string())
        .await
        .unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Validation);

    agent
        .respond(&client, "Что в итоге отправляем?".to_string())
        .await
        .unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Done);
    assert_eq!(task.expected_action, "none");
}

#[tokio::test]
async fn stage_marker_does_not_override_clear_planning_intent() {
    let client = MarkerClient::new("План дополнен. <<STAGE_DONE>>");
    let mut agent = agent_in_stage(TaskStage::Planning);

    agent
        .respond(
            &client,
            "Нашёл реквизиты договора для текущего плана.".to_string(),
        )
        .await
        .unwrap();

    assert_eq!(current_stage(&agent), TaskStage::Planning);
}

#[tokio::test]
async fn paused_task_resumes_on_continue_intent() {
    let client = MarkerClient::new("Продолжаю работу.");
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(TaskContext {
        stage: TaskStage::Execution,
        paused: true,
        resume_hint: "continue".to_string(),
        ..TaskContext::default()
    }));

    agent
        .respond(&client, "продолжай".to_string())
        .await
        .unwrap();
    assert!(!agent.task_state().expect("task").paused);
}

#[tokio::test]
async fn paused_task_does_not_advance_even_with_marker() {
    let client = MarkerClient::new("Готово. <<STAGE_DONE>>");
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(TaskContext {
        stage: TaskStage::Execution,
        paused: true,
        ..TaskContext::default()
    }));

    // Neutral prompt: stays paused, so the marker must not move the stage.
    agent.respond(&client, "хм".to_string()).await.unwrap();
    assert_eq!(current_stage(&agent), TaskStage::Execution);
    assert!(agent.task_state().expect("task").paused);
}

#[tokio::test]
async fn stage_responder_uses_its_own_model() {
    // Give the Execution stage agent its own model and confirm the responder
    // request at the Execution stage uses it.
    let mut config = SwarmConfig::defaults();
    config.set(SubAgentConfig {
        model: "exec-model".to_string(),
        ..SubAgentConfig::inherit(SubAgentRole::Execution)
    });
    let resolved = resolve_swarm(
        &test_profile(),
        "secret",
        &config,
        &AppConfig::default(),
        &MemorySecretStore::default(),
    );
    let client = MarkerClient::new("Шаг выполнен. <<STAGE_DONE>>");
    let mut agent = agent_in_stage(TaskStage::Execution);
    agent.set_swarm(resolved);

    agent.respond(&client, "Делай".to_string()).await.unwrap();
    let models = client.seen_models.lock().expect("models");
    assert!(
        models.iter().any(|model| model == "exec-model"),
        "execution responder must use its own model, saw {models:?}"
    );
}

#[tokio::test]
async fn explicit_execution_intent_routes_current_turn_to_execution_responder() {
    let mut config = SwarmConfig::defaults();
    config.set(SubAgentConfig {
        model: "execution-intent-model".to_string(),
        ..SubAgentConfig::inherit(SubAgentRole::Execution)
    });
    let resolved = resolve_swarm(
        &test_profile(),
        "secret",
        &config,
        &AppConfig::default(),
        &MemorySecretStore::default(),
    );
    let client = MarkerClient::new("Черновик с плейсхолдерами.");
    let mut agent = agent_in_stage(TaskStage::Planning);
    agent.set_swarm(resolved);

    agent
        .respond(&client, "Можно набросать сам текст?".to_string())
        .await
        .unwrap();

    let models = client.seen_models.lock().expect("models");
    assert!(
        models.iter().any(|model| model == "execution-intent-model"),
        "current execution intent must route to execution responder, saw {models:?}"
    );
}

#[tokio::test]
async fn repeated_execution_intent_does_not_advance_on_stage_marker() {
    let client = MarkerClient::new("Черновик обновлён. <<STAGE_DONE>>");
    let mut agent = agent_in_stage(TaskStage::Execution);

    agent
        .respond(&client, "Можно уже набросать сам текст?".to_string())
        .await
        .unwrap();

    assert_eq!(current_stage(&agent), TaskStage::Execution);
}
