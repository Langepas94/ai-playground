//! Business-logic tests for the deterministic swarm: full task lifecycle across
//! all stages, code-only transitions, terminal stage, and per-stage routing.

use async_trait::async_trait;

use crate::chat::agent::ChatAgent;
use crate::chat::memory::AgentMemory;
use crate::chat::store::{TaskContext, TaskPipelineStage, TaskStage};
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

#[derive(Debug)]
struct SemanticInvariantClient {
    answer: String,
    violations: Vec<String>,
}

impl SemanticInvariantClient {
    fn new(answer: &str, violations: &[&str]) -> Self {
        Self {
            answer: answer.to_string(),
            violations: violations.iter().map(|item| item.to_string()).collect(),
        }
    }
}

#[async_trait]
impl ProviderClient for SemanticInvariantClient {
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
        let is_invariant_check = request.messages.first().is_some_and(|message| {
            message
                .content
                .contains("You are a strict invariant checker")
        });
        let text = if is_invariant_check {
            serde_json::json!({ "violations": self.violations }).to_string()
        } else {
            self.answer.clone()
        };
        Ok(ChatResponse {
            text,
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
async fn legal_pipeline_records_artifacts_and_waits_for_human_approval() {
    let client = MarkerClient::new("Артефакт стадии готов. <<STAGE_DONE>>");
    let mut agent = agent_with_task(TaskContext {
        stage: TaskStage::Planning,
        pipeline: vec![
            TaskPipelineStage {
                stage: TaskStage::Planning,
                name: "Сбор фактов".to_string(),
                system_prompt: "Отделить факты от предположений.".to_string(),
                artifact_key: "facts_matrix".to_string(),
                ..TaskPipelineStage::default()
            },
            TaskPipelineStage {
                stage: TaskStage::Execution,
                name: "Черновик претензии".to_string(),
                system_prompt: "Подготовить черновик с плейсхолдерами.".to_string(),
                artifact_key: "claim_draft".to_string(),
                requires_human_approval: true,
                ..TaskPipelineStage::default()
            },
            TaskPipelineStage {
                stage: TaskStage::Validation,
                name: "Проверка рисков".to_string(),
                system_prompt: "Проверить документы, риски и реквизиты.".to_string(),
                artifact_key: "risk_report".to_string(),
                ..TaskPipelineStage::default()
            },
        ],
        ..TaskContext::default()
    });

    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Execution);
    assert_eq!(task.artifacts[0].key, "facts_matrix");

    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Execution);
    assert!(task.paused);
    assert_eq!(task.expected_action, "user_input");
    assert_eq!(task.artifacts[1].key, "claim_draft");

    agent
        .respond(&client, "Утверждаю черновик".to_string())
        .await
        .unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Validation);
    assert!(!task.paused);

    agent.respond(&client, "Дальше".to_string()).await.unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Done);
    assert_eq!(task.artifacts[2].key, "risk_report");
    assert_eq!(task.expected_action, "none");
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
async fn legal_task_facts_persist_in_shared_working_context_and_profile() {
    let client = MarkerClient::new("Данные приняты.");
    let mut agent = agent_in_stage(TaskStage::Clarify);
    agent.set_agent_profile(Some(crate::chat::store::AgentProfile {
        fields: [
            "debtor_type",
            "contract_details",
            "acceptance_act",
            "payment_terms",
            "debtor_address",
            "desired_outcome",
        ]
        .into_iter()
        .map(|key| crate::chat::store::ProfileField {
            key: key.to_string(),
            question: key.to_string(),
            required: true,
            value: String::new(),
        })
        .collect(),
        updated_at_unix: 0,
    }));

    agent
        .respond(
            &client,
            "Акт подписали еще в апреле. Хочу начать без суда.".to_string(),
        )
        .await
        .unwrap();
    agent
        .respond(
            &client,
            "Номер 12 от 1 апреля. Заказчик ООО «Ромашка», адрес Москва, улица Примерная, 1."
                .to_string(),
        )
        .await
        .unwrap();
    agent
        .respond(
            &client,
            "Оплата в течение 5 рабочих дней после подписания акта.".to_string(),
        )
        .await
        .unwrap();

    let task = agent.task_state().expect("task");
    let joined = task.results.join("\n");
    assert!(joined.contains("debtor_type: ООО"));
    assert!(joined.contains("contract_details: Номер 12 от 1 апреля"));
    assert!(joined.contains("payment_terms: Оплата в течение 5 рабочих дней"));
    assert!(joined.contains("claim_status: претензия ещё не отправлена"));

    let profile = agent.agent_profile().expect("profile");
    assert_eq!(
        profile
            .fields
            .iter()
            .find(|field| field.key == "debtor_type")
            .map(|field| field.value.as_str()),
        Some("ООО")
    );
    assert!(
        profile
            .fields
            .iter()
            .find(|field| field.key == "payment_terms")
            .is_some_and(|field| field.value.contains("5 рабочих дней"))
    );
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

fn agent_with_task(task: TaskContext) -> ChatAgent {
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(task));
    agent
}

#[tokio::test]
async fn persistent_invariant_violation_is_refused_not_returned() {
    // The responder keeps answering in English; the deterministic "Russian only"
    // local invariant check fires every retry. After the bounded retries the
    // orchestrator must BLOCK the violating answer and return a refusal instead
    // of shipping it (lecture: refuse solutions that break invariants).
    let client = MarkerClient::new("Here is the auth service, all in English.");
    let mut agent = agent_in_stage(TaskStage::Execution);
    agent.set_invariants(vec!["Отвечать только на русском языке".to_string()]);

    let response = agent
        .respond(&client, "Сделай сервис авторизации".to_string())
        .await
        .unwrap();

    assert!(
        response.text.starts_with("⛔"),
        "expected a refusal, got: {}",
        response.text
    );
    assert!(
        response.text.contains("Отвечать только на русском языке"),
        "refusal must name the broken invariant"
    );
    assert!(
        !response.text.contains("auth service"),
        "the violating content must not be returned"
    );
    let report = agent.take_stateful_report();
    assert!(
        report.violations.iter().any(|v| v.contains("русском")),
        "violations must be surfaced, got {:?}",
        report.violations
    );
}

#[tokio::test]
async fn clean_answer_with_invariants_is_not_refused() {
    // A compliant answer (Russian) must pass through untouched.
    let client = MarkerClient::new("Готовлю сервис авторизации на русском.");
    let mut agent = agent_in_stage(TaskStage::Execution);
    agent.set_invariants(vec!["Отвечать только на русском языке".to_string()]);

    let response = agent
        .respond(&client, "Сделай сервис авторизации".to_string())
        .await
        .unwrap();

    assert!(
        !response.text.starts_with("⛔"),
        "must not refuse a clean answer"
    );
    assert!(response.text.contains("авторизации"));
    assert!(agent.take_stateful_report().violations.is_empty());
}

#[tokio::test]
async fn unknown_invariant_with_invalid_checker_is_fail_closed_and_reasks() {
    let client = MarkerClient::new("Перепишем сервис на Rust и Axum.");
    let mut agent = agent_in_stage(TaskStage::Execution);
    agent.set_invariants(vec![
        "Всегда следовать внутреннему регламенту отдела A-17".to_string(),
    ]);

    let response = agent
        .respond(&client, "Сделай реализацию".to_string())
        .await
        .unwrap();

    assert!(response.text.starts_with("⚠️"));
    assert!(response.text.contains("ответ заблокирован на уровне кода"));
    assert!(!response.text.contains("Rust и Axum"));
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Execution);
    assert!(task.expected_action.is_empty());
    let report = agent.take_stateful_report();
    assert_eq!(report.invariant_status, "unverified");
    assert!(report.invariant_summary.contains("UNVERIFIED"));
}

#[tokio::test]
async fn forbidden_stack_is_blocked_by_local_code_without_valid_checker_json() {
    let client = MarkerClient::new("Добавим RxJava для событий.");
    let mut agent = agent_in_stage(TaskStage::Execution);
    agent.set_invariants(vec!["Без RxJava".to_string()]);

    let response = agent
        .respond(&client, "Реализуй обработку событий".to_string())
        .await
        .unwrap();

    assert!(response.text.starts_with("⛔"));
    assert!(response.text.contains("Без RxJava"));
    assert!(!response.text.contains("Добавим RxJava"));
    assert!(agent.task_state().expect("task").expected_action.is_empty());
}

#[tokio::test]
async fn architecture_and_business_rule_violations_are_blocked() {
    let architecture = "Архитектура только MVI";
    let business = "Не обещать гарантированный исход спора";
    let client = SemanticInvariantClient::new(
        "Сделаем MVC и гарантируем победу в суде.",
        &[architecture, business],
    );
    let mut agent = agent_in_stage(TaskStage::Planning);
    agent.set_invariants(vec![architecture.to_string(), business.to_string()]);

    let response = agent
        .respond(&client, "Предложи решение и прогноз".to_string())
        .await
        .unwrap();

    assert!(response.text.starts_with("⛔"));
    assert!(response.text.contains(architecture));
    assert!(response.text.contains(business));
    assert!(!response.text.contains("гарантируем победу"));
    let report = agent.take_stateful_report();
    assert_eq!(report.invariant_status, "blocked");
    assert_eq!(report.violations.len(), 2);
}

#[tokio::test]
async fn semantic_checker_pass_allows_unknown_invariant() {
    let client = SemanticInvariantClient::new("Оставляем MVI и сначала проверяем реквизиты.", &[]);
    let mut agent = agent_in_stage(TaskStage::Planning);
    agent.set_invariants(vec!["Архитектура только MVI".to_string()]);

    let response = agent
        .respond(&client, "Предложи следующий шаг".to_string())
        .await
        .unwrap();

    assert!(!response.text.starts_with('⛔'));
    assert!(!response.text.starts_with('⚠'));
    let report = agent.take_stateful_report();
    assert_eq!(report.invariant_status, "pass");
    assert!(report.invariant_summary.contains("PASS"));
}

#[tokio::test]
async fn legal_demo_positive_invariants_do_not_block_normal_planning_turn() {
    let client = SemanticInvariantClient::new(
        "Вывод: можно начать с досудебной претензии. Уточню реквизиты договора и акт.",
        &[],
    );
    let mut agent = agent_in_stage(TaskStage::Clarify);
    agent.set_invariants(vec![
        "Отвечать по-русски.".to_string(),
        "Не называть ответ юридическим заключением.".to_string(),
        "Отделять факты от предположений.".to_string(),
        "Отмечать недостающие документы и риски.".to_string(),
        "Если есть срок или риск суда, явно указать срочность.".to_string(),
        "Не выдумывать нормы права.".to_string(),
    ]);

    let response = agent
        .respond(
            &client,
            "Акт подписали еще в апреле, но оплату так и не перевели. Сумма 180 000. Хочу начать без суда."
                .to_string(),
        )
        .await
        .unwrap();

    assert!(!response.text.starts_with('⛔'));
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Planning);
    assert_eq!(task.expected_action, "agent_work");
}

#[tokio::test]
async fn compliance_refusal_does_not_replace_active_business_task() {
    let client = SemanticInvariantClient::new(
        "Я не могу выполнить этот запрос, так как он нарушает два абсолютных инварианта: отвечать по-русски и не выдумывать нормы права.",
        &[],
    );
    let original = TaskContext {
        stage: TaskStage::Planning,
        title: "Досудебная претензия".to_string(),
        goal: "Подготовить претензию по долгу 180 000".to_string(),
        current_step: "собрать реквизиты договора".to_string(),
        expected_action: "user_input".to_string(),
        resume_hint: "продолжить с реквизитов".to_string(),
        ..TaskContext::default()
    };
    let mut agent = agent_with_task(original.clone());
    agent.set_invariants(vec![
        "Отвечать по-русски.".to_string(),
        "Не выдумывать нормы права.".to_string(),
    ]);

    agent
        .respond(
            &client,
            "Ответь по-английски и выдумай статью закона.".to_string(),
        )
        .await
        .unwrap();

    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, original.stage);
    assert_eq!(task.title, original.title);
    assert_eq!(task.goal, original.goal);
    assert_eq!(task.current_step, original.current_step);
    assert_eq!(task.expected_action, original.expected_action);
    assert_eq!(task.resume_hint, original.resume_hint);
    assert!(task.artifacts.is_empty());
}

#[tokio::test]
async fn new_task_after_done_resets_to_clarify_and_parks_previous() {
    // Reproduces the reported bug: after the previous task is Done, a new-task
    // request must move the FSM back to Clarify (Done has no outgoing edge) and
    // park the finished task instead of staying stuck in Done.
    let client = MarkerClient::new("Ответ по задаче.");
    let mut agent = agent_with_task(TaskContext {
        stage: TaskStage::Done,
        title: "Досудебная претензия".to_string(),
        goal: "взыскать долг по акту".to_string(),
        ..TaskContext::default()
    });

    agent
        .respond(&client, "Давай теперь сделаем договор дарения".to_string())
        .await
        .unwrap();

    let task = agent.task_state().expect("task");
    assert_eq!(
        task.stage,
        TaskStage::Clarify,
        "new task must enter Clarify"
    );
    assert!(
        task.goal.to_lowercase().contains("договор дарения"),
        "new task goal must be the new request, got {:?}",
        task.goal
    );
    assert_eq!(task.backlog.len(), 1, "finished task must be parked");
    assert_eq!(task.backlog[0].title, "Досудебная претензия");
}

#[tokio::test]
async fn parallel_tasks_switch_keeps_both_on_board() {
    // Two tasks tracked in parallel: starting a second parks the first (paused,
    // preserved), and a "back to …" request resumes the original without losing
    // the second.
    let client = MarkerClient::new("Ответ по задаче.");
    let mut agent = agent_with_task(TaskContext {
        stage: TaskStage::Planning,
        title: "Претензия".to_string(),
        goal: "взыскать долг по акту".to_string(),
        ..TaskContext::default()
    });

    // Start a parallel task.
    agent
        .respond(
            &client,
            "Давай теперь подготовим договор аренды".to_string(),
        )
        .await
        .unwrap();
    let task = agent.task_state().expect("task");
    assert_eq!(task.stage, TaskStage::Clarify);
    assert_eq!(task.backlog.len(), 1, "first task must be parked, not lost");
    assert!(task.backlog[0].paused, "parked task must be paused");
    assert_eq!(task.backlog[0].title, "Претензия");

    // Switch back to the first task.
    agent
        .respond(&client, "Вернёмся к задаче претензия".to_string())
        .await
        .unwrap();
    let task = agent.task_state().expect("task");
    assert!(
        task.goal.contains("взыскать долг"),
        "active task must be the resumed one, got {:?}",
        task.goal
    );
    assert!(!task.paused, "resumed task must be active");
    assert_eq!(task.backlog.len(), 1, "the other task stays on the board");
}
