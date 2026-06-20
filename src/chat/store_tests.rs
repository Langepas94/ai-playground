use super::memory::{AgentMemory, MemoryLayer, TopicFile};
use super::store::*;
use crate::errors::AppError;
use crate::providers::{ChatMessage, CostSource, RequestCost, RequestMetrics, Role, TokenUsage};

#[test]
fn local_session_store_roundtrips_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("create session");
    let messages = vec![
        ChatMessage {
            role: Role::User,
            content: "hello".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "hi".to_string(),
        },
    ];

    store
        .save_session("profile:model", &session.id, &messages)
        .expect("save session");
    let loaded = store
        .load_or_create_latest("profile:model")
        .expect("load latest");

    assert_eq!(loaded.id, session.id);
    assert_eq!(loaded.messages, messages);
    assert_eq!(loaded.metrics, RequestMetrics::default());
}

#[test]
fn long_term_survives_new_session_short_term_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let profile = "profile:model";

    // Session 1: a long-term fact + short-term summary.
    let mut memory = AgentMemory::default();
    memory.set_fact_in_layer(
        "preferences".to_string(),
        "concise answers".to_string(),
        MemoryLayer::LongTerm,
    );
    memory.set_fact_in_layer(
        "goal".to_string(),
        "ship feature".to_string(),
        MemoryLayer::Working,
    );
    memory.session_summary = Some("earlier discussion".to_string());
    store
        .save_long_term(profile, &memory)
        .expect("save long-term");

    // Session 2: brand-new memory, only long-term seeded.
    let mut fresh = AgentMemory::default();
    store.seed_long_term(profile, &mut fresh).expect("seed");

    assert_eq!(
        fresh.facts.get("preferences").map(String::as_str),
        Some("concise answers"),
        "long-term fact must carry into a new session"
    );
    assert_eq!(fresh.fact_layer("preferences"), MemoryLayer::LongTerm);
    assert!(
        !fresh.facts.contains_key("goal"),
        "working-layer fact must not leak into a new session"
    );
    assert!(
        fresh.session_summary.is_none(),
        "short-term summary must be empty in a new session"
    );
}

#[test]
fn agent_long_term_facts_are_shared_between_dialogs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let agent_key = "agent:menu-agent";

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
        .save_long_term(agent_key, &first_dialog_memory)
        .expect("save agent long-term");

    let mut second_dialog_memory = AgentMemory::default();
    store
        .seed_long_term(agent_key, &mut second_dialog_memory)
        .expect("seed agent long-term");

    assert_eq!(
        second_dialog_memory
            .facts
            .get("available_products")
            .map(String::as_str),
        Some("tomatoes, basil, mozzarella"),
        "manually saved long-term facts should be visible in another dialog"
    );
    assert_eq!(
        second_dialog_memory.fact_layer("available_products"),
        MemoryLayer::LongTerm
    );
    assert!(
        !second_dialog_memory.facts.contains_key("current_task"),
        "working facts should not leak between dialogs"
    );
}

#[test]
fn saved_agents_roundtrip_list_and_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));

    let agent = SavedAgent {
        id: "a1".to_string(),
        name: "Rust reviewer".to_string(),
        provider: "openai-compatible".to_string(),
        model: "gpt-4.1-mini".to_string(),
        system_prompt: "be terse".to_string(),
        updated_at_unix: 100,
        ..SavedAgent::default()
    };
    store.save_agent(&agent).expect("save agent");

    // Working (task + stage) + long-term (profile) layers.
    let task = TaskContext {
        stage: TaskStage::Planning,
        current_step: "draft the storage model".to_string(),
        expected_action: "agent_work".to_string(),
        paused: true,
        resume_hint: "continue from TaskContext serialization".to_string(),
        title: "ship agents".to_string(),
        goal: "persist settings".to_string(),
        plan: vec!["design".to_string(), "build".to_string()],
        ..TaskContext::default()
    };
    store.save_task("a1", &task).expect("save task");
    store
        .save_profile(
            "a1",
            &AgentProfile {
                fields: vec![ProfileField {
                    key: "stack".to_string(),
                    question: "Which stack?".to_string(),
                    required: true,
                    value: "Rust".to_string(),
                }],
                updated_at_unix: 100,
            },
        )
        .expect("save profile");

    let loaded = store.load_agent("a1").expect("load").expect("present");
    assert_eq!(loaded.name, "Rust reviewer");
    assert_eq!(loaded.system_prompt, "be terse");
    let loaded_task = store.load_task("a1").expect("task");
    assert_eq!(loaded_task.title, "ship agents");
    assert_eq!(loaded_task.stage, TaskStage::Planning);
    assert_eq!(loaded_task.current_step, "draft the storage model");
    assert_eq!(loaded_task.expected_action, "agent_work");
    assert!(loaded_task.paused);
    assert_eq!(
        loaded_task.resume_hint,
        "continue from TaskContext serialization"
    );
    let loaded_profile = store.load_profile("a1").expect("profile");
    assert_eq!(loaded_profile.fields[0].value, "Rust");
    assert!(loaded_profile.pending_required().is_empty());

    let listed = store.list_agents().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "a1");
    // Index stage stays in sync with the saved task.
    assert_eq!(listed[0].stage, TaskStage::Planning);

    store.delete_agent("a1").expect("delete");
    assert!(store.load_agent("a1").expect("load after delete").is_none());
    assert!(store.list_agents().expect("list after delete").is_empty());
}

#[test]
fn user_profiles_are_reusable_runtime_bindings_not_agent_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    store
        .save_agent(&SavedAgent {
            id: "coding-agent".to_string(),
            name: "Coding".to_string(),
            updated_at_unix: 1,
            ..SavedAgent::default()
        })
        .expect("save coding agent");
    store
        .save_agent(&SavedAgent {
            id: "research-agent".to_string(),
            name: "Research".to_string(),
            updated_at_unix: 2,
            ..SavedAgent::default()
        })
        .expect("save research agent");

    let profile = UserProfile {
        id: "artem-short-russian".to_string(),
        display_name: "Artem short Russian".to_string(),
        style_preferences: vec!["short".to_string()],
        language_preferences: vec!["Russian".to_string()],
        updated_at_unix: 10,
        ..UserProfile::default()
    };
    store.save_user_profile(&profile).expect("save profile");
    let mut bindings = UserProfileBindings {
        active_profile_id: profile.id.clone(),
        ..UserProfileBindings::default()
    };
    bindings
        .default_profile_per_agent
        .insert("research-agent".to_string(), profile.id.clone());
    store
        .save_user_profile_bindings(&bindings)
        .expect("save bindings");

    let coding_profile = store
        .resolve_user_profile(None, Some("coding-agent"))
        .expect("resolve coding")
        .expect("coding profile");
    let research_profile = store
        .resolve_user_profile(None, Some("research-agent"))
        .expect("resolve research")
        .expect("research profile");
    assert_eq!(coding_profile.id, profile.id);
    assert_eq!(research_profile.id, profile.id);

    store.delete_agent("research-agent").expect("delete agent");
    assert!(
        store
            .load_user_profile("artem-short-russian")
            .expect("load profile")
            .is_some(),
        "deleting an agent must not delete a reusable user profile"
    );
    assert!(
        store
            .load_agent("coding-agent")
            .expect("load coding")
            .expect("coding present")
            .system_prompt
            .is_empty(),
        "agent definition must not duplicate user profile preferences"
    );
}

#[test]
fn user_profile_resolution_priority_is_explicit_agent_default_then_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    for id in ["active", "agent-default", "explicit"] {
        store
            .save_user_profile(&UserProfile {
                id: id.to_string(),
                display_name: id.to_string(),
                updated_at_unix: 1,
                ..UserProfile::default()
            })
            .expect("save profile");
    }
    let mut bindings = UserProfileBindings {
        active_profile_id: "active".to_string(),
        ..UserProfileBindings::default()
    };
    bindings
        .default_profile_per_agent
        .insert("coding-agent".to_string(), "agent-default".to_string());
    store
        .save_user_profile_bindings(&bindings)
        .expect("save bindings");

    assert_eq!(
        store
            .resolve_user_profile(Some("explicit"), Some("coding-agent"))
            .expect("explicit")
            .expect("profile")
            .id,
        "explicit",
        "explicit runtime selection must win"
    );
    assert_eq!(
        store
            .resolve_user_profile(None, Some("coding-agent"))
            .expect("agent default")
            .expect("profile")
            .id,
        "agent-default",
        "agent default is selection metadata, not ownership"
    );
    assert_eq!(
        store
            .resolve_user_profile(None, Some("other-agent"))
            .expect("active")
            .expect("profile")
            .id,
        "active",
        "active profile is the fallback"
    );
    assert!(
        store
            .resolve_user_profile(Some("missing"), Some("coding-agent"))
            .expect("missing explicit")
            .is_none(),
        "a missing explicit profile must not silently fall back to another profile"
    );
}

#[test]
fn task_stage_transitions_enforced() {
    // Legal forward transitions.
    assert!(TaskStage::Clarify.can_transition(TaskStage::Planning));
    assert!(TaskStage::Planning.can_transition(TaskStage::Execution));
    assert!(TaskStage::Execution.can_transition(TaskStage::Validation));
    assert!(TaskStage::Validation.can_transition(TaskStage::Done));
    // Legal back-transitions.
    assert!(TaskStage::Execution.can_transition(TaskStage::Planning));
    assert!(TaskStage::Validation.can_transition(TaskStage::Execution));
    // Staying put is allowed.
    assert!(TaskStage::Planning.can_transition(TaskStage::Planning));
    // Illegal jumps are rejected.
    assert!(!TaskStage::Clarify.can_transition(TaskStage::Execution));
    assert!(!TaskStage::Clarify.can_transition(TaskStage::Done));
    assert!(!TaskStage::Planning.can_transition(TaskStage::Validation));
    assert!(!TaskStage::Planning.can_transition(TaskStage::Done));
    assert!(!TaskStage::Execution.can_transition(TaskStage::Done));
    assert!(!TaskStage::Done.can_transition(TaskStage::Planning));
    assert!(TaskStage::Done.allowed_next().is_empty());
    // Round-trips through its string form.
    assert_eq!("execution".parse(), Ok(TaskStage::Execution));
    assert_eq!(TaskStage::Validation.to_string(), "validation");
}

#[test]
fn allowed_next_table_is_the_only_source_of_legal_transitions() {
    // Exhaustive property: for EVERY ordered stage pair, `can_transition` must
    // agree with the `allowed_next` table (plus the always-legal self-loop).
    // This locks the table against accidental widening — e.g. someone adding a
    // skip edge would have to update this test deliberately.
    for &from in &TaskStage::ORDERED {
        for &to in &TaskStage::ORDERED {
            let expected = from == to || from.allowed_next().contains(&to);
            assert_eq!(
                from.can_transition(to),
                expected,
                "{from} -> {to}: can_transition must match allowed_next"
            );
        }
    }

    // A rejected skip must NOT mutate the stage (the FSM stays put on refusal).
    let mut clarify = TaskContext {
        stage: TaskStage::Clarify,
        ..TaskContext::default()
    };
    let jump = clarify.try_transition(TaskStage::Execution);
    assert!(!jump.accepted);
    assert!(jump.reason.contains("запрещён"));
    assert_eq!(clarify.stage, TaskStage::Clarify);

    let mut planning = TaskContext {
        stage: TaskStage::Planning,
        plan_approved: true,
        ..TaskContext::default()
    };
    // Planning -> Done is not in the table at all: rejected before any gate runs.
    let jump = planning.try_transition(TaskStage::Done);
    assert!(!jump.accepted);
    assert!(jump.reason.contains("запрещён"));
    assert_eq!(planning.stage, TaskStage::Planning);
}

#[test]
fn lifecycle_guards_require_artifacts_approval_and_validation() {
    let mut task = TaskContext {
        stage: TaskStage::Planning,
        ..TaskContext::default()
    };
    let rejected = task.try_transition(TaskStage::Execution);
    assert!(!rejected.accepted);
    assert!(rejected.reason.contains("planning"));

    task.record_stage_artifact(TaskStage::Planning, "plan", "1. Build\n2. Test");
    let rejected = task.try_transition(TaskStage::Execution);
    assert!(!rejected.accepted);
    assert!(rejected.reason.contains("утвердить"));

    task.plan_approved = true;
    assert!(task.try_transition(TaskStage::Execution).accepted);

    let rejected = task.try_transition(TaskStage::Validation);
    assert!(!rejected.accepted);
    assert!(rejected.reason.contains("execution"));

    task.record_stage_artifact(TaskStage::Execution, "result", "implemented");
    assert!(task.try_transition(TaskStage::Validation).accepted);

    let rejected = task.try_transition(TaskStage::Done);
    assert!(!rejected.accepted);
    assert!(rejected.reason.contains("validation"));

    task.record_stage_artifact(TaskStage::Validation, "validation", "passed");
    task.validation_passed = true;
    assert!(task.try_transition(TaskStage::Done).accepted);
}

#[test]
fn invalid_persisted_lifecycle_is_rewound_to_missing_prerequisite() {
    let mut execution = TaskContext {
        stage: TaskStage::Execution,
        ..TaskContext::default()
    };
    let reason = execution
        .repair_lifecycle_integrity()
        .expect("invalid execution");
    assert_eq!(execution.stage, TaskStage::Planning);
    assert!(reason.contains("без утверждённого"));

    let mut done = TaskContext {
        stage: TaskStage::Done,
        plan_approved: true,
        validation_passed: false,
        artifacts: vec![
            TaskArtifact {
                stage: TaskStage::Planning,
                key: "plan".to_string(),
                value: "approved".to_string(),
            },
            TaskArtifact {
                stage: TaskStage::Execution,
                key: "result".to_string(),
                value: "implemented".to_string(),
            },
        ],
        ..TaskContext::default()
    };
    let reason = done.repair_lifecycle_integrity().expect("invalid done");
    assert_eq!(done.stage, TaskStage::Validation);
    assert!(!done.validation_passed);
    assert!(reason.contains("validation"));
}

#[test]
fn task_context_pause_resume_preserves_formal_state() {
    for stage in [
        TaskStage::Clarify,
        TaskStage::Planning,
        TaskStage::Execution,
        TaskStage::Validation,
    ] {
        let mut task = TaskContext {
            stage,
            current_step: "run validation checks".to_string(),
            expected_action: "agent_work".to_string(),
            ..TaskContext::default()
        };

        assert!(task.pause("continue from cargo test failure analysis"));
        assert_eq!(task.stage, stage);
        assert!(task.paused);
        assert_eq!(task.current_step, "run validation checks");
        assert_eq!(task.expected_action, "agent_work");
        assert_eq!(
            task.resume_hint,
            "continue from cargo test failure analysis"
        );

        assert!(task.resume());
        assert!(!task.paused);
        assert_eq!(task.stage, stage);
        assert_eq!(task.current_step, "run validation checks");
        assert_eq!(task.expected_action, "agent_work");
    }

    let mut done = TaskContext {
        stage: TaskStage::Done,
        ..TaskContext::default()
    };
    assert!(!done.pause("already complete"));
    assert!(!done.paused);
}

#[test]
fn pipeline_stage_artifact_drives_deterministic_transition_or_human_pause() {
    let mut task = TaskContext {
        stage: TaskStage::Planning,
        current_step: "run research workers".to_string(),
        expected_action: "agent_work".to_string(),
        pipeline: vec![
            TaskPipelineStage {
                stage: TaskStage::Planning,
                name: "Research".to_string(),
                system_prompt: "Research legal context before drafting.".to_string(),
                artifact_key: "research_conclusion".to_string(),
                worker_agents: vec![
                    TaskWorkerAgent {
                        id: "ru-law".to_string(),
                        direction: "russian_law".to_string(),
                        system_prompt: "Analyze Russian contract law.".to_string(),
                    },
                    TaskWorkerAgent {
                        id: "patent-law".to_string(),
                        direction: "patent".to_string(),
                        system_prompt: "Analyze patent/IP constraints.".to_string(),
                    },
                ],
                ..TaskPipelineStage::default()
            },
            TaskPipelineStage {
                stage: TaskStage::Execution,
                name: "Create".to_string(),
                system_prompt: "Create the contract from approved research.".to_string(),
                artifact_key: "contract_draft".to_string(),
                requires_human_approval: true,
                ..TaskPipelineStage::default()
            },
            TaskPipelineStage {
                stage: TaskStage::Validation,
                name: "Validation".to_string(),
                system_prompt: "Validate the contract against requirements.".to_string(),
                artifact_key: "validation_report".to_string(),
                ..TaskPipelineStage::default()
            },
        ],
        ..TaskContext::default()
    };

    let advance = task
        .complete_pipeline_stage("research_conclusion", "Russian law ok; patent risk noted")
        .expect("research stage");
    assert_eq!(advance.from, TaskStage::Planning);
    assert_eq!(advance.to, TaskStage::Planning);
    assert!(advance.accepted);
    assert!(advance.paused_for_human);
    assert_eq!(task.stage, TaskStage::Planning);
    assert!(task.paused);
    assert_eq!(task.artifacts.len(), 1);

    assert!(task.approve_pipeline_pause());
    assert_eq!(task.stage, TaskStage::Execution);
    assert!(task.plan_approved);
    assert!(!task.paused);

    let advance = task
        .complete_pipeline_stage("contract_draft", "Draft contract text")
        .expect("create stage");
    assert_eq!(advance.from, TaskStage::Execution);
    assert_eq!(advance.to, TaskStage::Execution);
    assert!(advance.accepted);
    assert!(advance.paused_for_human);
    assert_eq!(task.stage, TaskStage::Execution);
    assert!(task.paused);
    assert_eq!(task.current_step, "approve Create artifact");
    assert_eq!(task.expected_action, "user_input");
    assert!(task.resume_hint.contains("Review contract_draft artifact"));

    assert!(task.approve_pipeline_pause());
    assert_eq!(task.stage, TaskStage::Validation);
    assert!(!task.paused);
    assert_eq!(task.current_step, "Validation");
    assert_eq!(task.expected_action, "agent_work");
}

#[test]
fn task_fsm_state_survives_restart_and_shared_dialog_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("sessions");
    let store = LocalSessionStore::from_root(root.clone());

    store
        .save_session(
            "agent:app-agent",
            "planning-window",
            &[ChatMessage {
                role: Role::User,
                content: "Хочу создать приложение".to_string(),
            }],
        )
        .expect("save planning window");
    store
        .save_dialog_task(
            "app-agent",
            "planning-window",
            &TaskContext {
                stage: TaskStage::Execution,
                current_step: "implement approved app scaffold".to_string(),
                expected_action: "agent_work".to_string(),
                paused: true,
                resume_hint: "app name approved; plan approved; continue implementation"
                    .to_string(),
                title: "create application".to_string(),
                goal: "build the approved application".to_string(),
                plan: vec![
                    "clarify app name".to_string(),
                    "approve implementation plan".to_string(),
                    "implement app scaffold".to_string(),
                    "run validation".to_string(),
                ],
                ..TaskContext::default()
            },
        )
        .expect("save task state");

    let restarted_store = LocalSessionStore::from_root(root);
    restarted_store
        .save_session(
            "agent:app-agent",
            "resume-window",
            &[ChatMessage {
                role: Role::User,
                content: "Продолжай".to_string(),
            }],
        )
        .expect("save resume window");

    let resumed = restarted_store
        .load_dialog_task("app-agent", "resume-window")
        .expect("load shared task after restart");
    assert_eq!(resumed.stage, TaskStage::Execution);
    assert_eq!(resumed.current_step, "implement approved app scaffold");
    assert_eq!(resumed.expected_action, "agent_work");
    assert!(resumed.paused);
    assert_eq!(
        resumed.resume_hint,
        "app name approved; plan approved; continue implementation"
    );
    assert_eq!(
        resumed.plan,
        vec![
            "clarify app name",
            "approve implementation plan",
            "implement app scaffold",
            "run validation"
        ]
    );
}

#[test]
fn stale_task_revision_is_rejected_instead_of_overwriting_newer_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let initial = TaskContext {
        goal: "shared task".to_string(),
        ..TaskContext::default()
    };
    store.save_task("agent", &initial).expect("initial save");

    let mut writer_a = store.load_task("agent").expect("writer a");
    let mut writer_b = store.load_task("agent").expect("writer b");
    writer_a.notes = "writer a".to_string();
    store.save_task("agent", &writer_a).expect("writer a save");

    writer_b.notes = "writer b".to_string();
    let error = store
        .save_task("agent", &writer_b)
        .expect_err("stale writer must conflict");
    assert!(error.to_string().contains("revision"));
    assert_eq!(store.load_task("agent").unwrap().notes, "writer a");
}

#[test]
fn unknown_serialized_task_stage_is_rejected() {
    let raw = r#"{"stage":"teleport","goal":"unsafe"}"#;
    assert!(serde_json::from_str::<TaskContext>(raw).is_err());
}

#[test]
fn agent_dialogs_roundtrip_and_auto_register() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));

    assert_eq!(agent_id_from_key("agent:a1"), Some("a1"));
    assert_eq!(agent_id_from_key("web:x:y:z"), None);

    // save_session under an agent key auto-registers a dialog with a title
    // derived from the first user message.
    let messages = vec![
        ChatMessage {
            role: Role::User,
            content: "Спланируй экран логина для приложения".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "ок".to_string(),
        },
    ];
    store
        .save_session("agent:a1", "sess-1", &messages)
        .expect("save session");
    store
        .save_session("agent:a1", "sess-2", &[])
        .expect("save empty session");
    let task_1 = TaskContext {
        title: "menu".to_string(),
        goal: "invent dishes".to_string(),
        ..TaskContext::default()
    };
    let task_2 = TaskContext {
        title: "slogan".to_string(),
        goal: "invent slogan".to_string(),
        ..TaskContext::default()
    };
    store
        .save_dialog_task("a1", "sess-1", &task_1)
        .expect("save shared default task");
    assert_eq!(
        store
            .load_dialog_task("a1", "sess-1")
            .expect("load dialog task 1")
            .goal,
        "invent dishes"
    );
    assert_eq!(
        store
            .load_dialog_task("a1", "sess-2")
            .expect("load shared task from dialog 2")
            .goal,
        "invent dishes"
    );
    store
        .assign_dialog_task("a1", "sess-2", "slogan")
        .expect("assign dialog 2 to another task");
    store
        .save_dialog_task("a1", "sess-2", &task_2)
        .expect("save dialog task 2");
    assert_eq!(store.dialog_task_id("a1", "sess-1").unwrap(), "default");
    assert_eq!(store.dialog_task_id("a1", "sess-2").unwrap(), "slogan");
    assert_eq!(
        store.load_dialog_task("a1", "sess-1").unwrap().goal,
        "invent dishes"
    );
    assert_eq!(
        store.load_dialog_task("a1", "sess-2").unwrap().goal,
        "invent slogan"
    );

    let dialogs = store.list_dialogs("a1").expect("list");
    assert_eq!(dialogs.len(), 2);
    let first = dialogs.iter().find(|d| d.id == "sess-1").expect("sess-1");
    assert!(first.title.starts_with("Спланируй экран логина"));

    store
        .rename_dialog("a1", "sess-1", "Логин")
        .expect("rename");
    assert_eq!(
        store
            .list_dialogs("a1")
            .unwrap()
            .iter()
            .find(|d| d.id == "sess-1")
            .unwrap()
            .title,
        "Логин"
    );

    store.delete_dialog("a1", "sess-2").expect("delete");
    let after = store.list_dialogs("a1").expect("list after delete");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, "sess-1");
    assert_eq!(
        store.load_scoped_task("a1", "slogan").unwrap().goal,
        "invent slogan",
        "deleting one dialog must not delete its task scope"
    );
    // Ad-hoc (non-agent) sessions are not registered as dialogs.
    store
        .save_session("web:local:openai:gpt", "sess-3", &messages)
        .expect("save adhoc");
    assert!(store.list_dialogs("other").unwrap().is_empty());
}

#[test]
fn local_session_store_roundtrips_metrics_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("create session");
    let metrics = RequestMetrics {
        elapsed_ms: 123,
        usage: Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            cache_hit_input_tokens: Some(4),
            cache_miss_input_tokens: Some(6),
            ..TokenUsage::default()
        }),
        cost: Some(RequestCost {
            amount: 0.00042,
            currency: "USD".to_string(),
            source: CostSource::ConfiguredPricing,
        }),
    };

    store
        .save_metrics(&session.id, &metrics)
        .expect("save metrics");
    let loaded = store.load_metrics(&session.id).expect("load metrics");

    assert_eq!(loaded, metrics);
}

#[test]
fn request_metrics_accumulate_usage_and_matching_costs() {
    let total = RequestMetrics {
        elapsed_ms: 100,
        usage: Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cache_hit_input_tokens: Some(3),
            cache_miss_input_tokens: None,
            output_reasoning_tokens: Some(4),
            output_visible_tokens: Some(1),
            ..TokenUsage::default()
        }),
        cost: Some(RequestCost {
            amount: 0.001,
            currency: "USD".to_string(),
            source: CostSource::ConfiguredPricing,
        }),
    };
    let request = RequestMetrics {
        elapsed_ms: 200,
        usage: Some(TokenUsage {
            input_tokens: 7,
            output_tokens: 11,
            total_tokens: 18,
            cache_hit_input_tokens: Some(2),
            cache_miss_input_tokens: Some(5),
            output_reasoning_tokens: Some(6),
            output_visible_tokens: Some(5),
            ..TokenUsage::default()
        }),
        cost: Some(RequestCost {
            amount: 0.002,
            currency: "USD".to_string(),
            source: CostSource::ConfiguredPricing,
        }),
    };

    let added = add_request_metrics(&total, &request);

    assert_eq!(added.elapsed_ms, 300);
    assert_eq!(
        added.usage,
        Some(TokenUsage {
            input_tokens: 17,
            output_tokens: 16,
            total_tokens: 33,
            cache_hit_input_tokens: Some(5),
            cache_miss_input_tokens: Some(5),
            output_reasoning_tokens: Some(10),
            output_visible_tokens: Some(6),
            ..TokenUsage::default()
        })
    );
    assert_eq!(
        added.cost,
        Some(RequestCost {
            amount: 0.003,
            currency: "USD".to_string(),
            source: CostSource::ConfiguredPricing,
        })
    );
}

#[test]
fn local_session_store_roundtrips_memory_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("create session");
    let memory = AgentMemory {
        facts: Default::default(),
        branch_assignments: Default::default(),
        session_summary: Some("User prefers short technical answers.".to_string()),
        summarized_message_count: 8,
        ..AgentMemory::default()
    };

    store
        .save_memory(&session.id, &memory)
        .expect("save memory");
    let loaded = store.load_memory(&session.id).expect("load memory");

    assert_eq!(loaded, memory);
}

#[test]
fn local_session_store_roundtrips_topic_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("create session");
    let topic_store = store.topic_file_storage(&session.id).expect("topic store");
    let topic = TopicFile {
        metadata: crate::chat::memory::TopicMetadata {
            id: "rust async".to_string(),
            title: "Rust async".to_string(),
            short_description: "Ownership and async context.".to_string(),
            tags: vec!["rust".to_string(), "async".to_string()],
            message_count: 4,
            updated_at_unix: 123,
        },
        context: "user: borrow checker\nassistant: use ownership boundaries".to_string(),
    };

    topic_store.save_topic_file(&topic).expect("save topic");
    let loaded = topic_store
        .load_topic_file("rust async")
        .expect("load topic")
        .expect("topic exists");

    assert_eq!(loaded, topic);
}

#[test]
fn local_session_store_rejects_path_like_session_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalSessionStore::from_root(dir.path().join("sessions"));

    assert!(matches!(
        store.load_session("../secret"),
        Err(AppError::InvalidInput(_))
    ));
    assert!(matches!(
        store.load_session(""),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn start_new_task_parks_previous_and_promotes_clarify() {
    let mut task = TaskContext {
        stage: TaskStage::Execution,
        title: "Претензия".to_string(),
        goal: "взыскать долг".to_string(),
        ..TaskContext::default()
    };
    task.start_new_task("сделать договор дарения", "продолжить с черновика");

    assert_eq!(task.stage, TaskStage::Clarify);
    assert_eq!(task.goal, "сделать договор дарения");
    assert_eq!(task.backlog.len(), 1);
    let parked = &task.backlog[0];
    assert_eq!(parked.title, "Претензия");
    assert!(parked.paused, "non-terminal parked task is paused");
    assert!(parked.backlog.is_empty(), "backlog must stay flat");
}

#[test]
fn start_new_task_does_not_park_an_empty_task() {
    let mut task = TaskContext::default();
    task.start_new_task("первая задача", "");
    assert!(task.backlog.is_empty(), "empty task is not worth parking");
    assert_eq!(task.goal, "первая задача");
}

#[test]
fn switch_to_backlog_swaps_active_and_parks_outgoing() {
    let mut task = TaskContext {
        stage: TaskStage::Clarify,
        goal: "договор аренды".to_string(),
        title: "Аренда".to_string(),
        backlog: vec![TaskContext {
            stage: TaskStage::Planning,
            title: "Претензия".to_string(),
            goal: "взыскать долг".to_string(),
            paused: true,
            ..TaskContext::default()
        }],
        ..TaskContext::default()
    };

    assert!(task.switch_to_backlog(0));
    assert_eq!(task.goal, "взыскать долг", "resumed task becomes active");
    assert!(!task.paused, "resumed task is no longer paused");
    assert_eq!(task.backlog.len(), 1);
    assert_eq!(task.backlog[0].title, "Аренда", "outgoing task is parked");
    assert!(task.backlog[0].paused);

    assert!(!task.switch_to_backlog(5), "out-of-range index is a no-op");
}
