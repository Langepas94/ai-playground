
use super::*;

fn message(role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: content.to_string(),
    }
}

#[test]
fn sliding_window_keeps_system_and_last_n_messages() {
    let memory = AgentMemory::default();
    let history = vec![
        message(Role::System, "Base system prompt"),
        message(Role::User, "old user"),
        message(Role::Assistant, "old assistant"),
        message(Role::User, "recent user"),
        message(Role::Assistant, "recent assistant"),
    ];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: 2,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context.len(), 3);
    assert_eq!(context[0].content, "Base system prompt");
    assert_eq!(context[1].content, "recent user");
    assert_eq!(context[2].content, "recent assistant");
}

#[test]
fn sliding_window_with_zero_recent_keeps_only_system_context() {
    let memory = AgentMemory::default();
    let history = vec![
        message(Role::System, "System stays"),
        message(Role::User, "old user"),
        message(Role::Assistant, "old assistant"),
    ];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: 0,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context, vec![message(Role::System, "System stays")]);
}

#[test]
fn sticky_facts_layers_facts_before_recent_messages() {
    let mut memory = AgentMemory::default();
    memory
        .facts
        .insert("goal".to_string(), "Ship context strategies".to_string());
    let history = vec![
        message(Role::User, "old user"),
        message(Role::Assistant, "old assistant"),
        message(Role::User, "recent user"),
    ];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 1,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context.len(), 2);
    assert_eq!(context[0].role, Role::System);
    assert!(context[0].content.contains("Read-only local memory facts"));
    assert!(context[0].content.contains("FACTS_KV:"));
    assert!(context[0].content.contains("goal: Ship context strategies"));
    assert_eq!(context[1].content, "recent user");
}

#[test]
fn sticky_facts_uses_custom_prompt_for_facts_block() {
    let mut memory = AgentMemory::default();
    memory
        .facts
        .insert("goal".to_string(), "make prompt configurable".to_string());
    let history = vec![message(Role::User, "recent user")];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 1,
            facts_prompt: "Custom facts instruction.".to_string(),
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context[0].role, Role::System);
    assert!(context[0].content.starts_with("Custom facts instruction."));
    assert!(
        !context[0]
            .content
            .starts_with("Read-only local memory facts")
    );
    assert!(context[0].content.contains("FACTS_KV:"));
    assert!(
        context[0]
            .content
            .contains("goal: make prompt configurable")
    );
}

#[test]
fn summary_context_layers_summary_before_unsummarized_tail() {
    let memory = AgentMemory {
        facts: Default::default(),
        branch_assignments: Default::default(),
        session_summary: Some("User likes concise answers.".to_string()),
        summarized_message_count: 3,
    };
    let history = vec![
        message(Role::System, "Base system prompt"),
        message(Role::User, "old user"),
        message(Role::Assistant, "old assistant"),
        message(Role::User, "recent user"),
        message(Role::Assistant, "recent assistant"),
    ];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::Summary,
            recent_messages: 2,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context.len(), 4);
    assert_eq!(context[0].content, "Base system prompt");
    assert!(context[1].content.contains("User likes concise answers."));
    assert_eq!(context[2].content, "recent user");
    assert_eq!(context[3].content, "recent assistant");
}

#[test]
fn summary_context_never_drops_messages_not_yet_summarized() {
    let memory = AgentMemory::default();
    let history = vec![
        message(Role::User, "FIRST important details"),
        message(Role::Assistant, "answer 1"),
        message(Role::User, "second"),
        message(Role::Assistant, "answer 2"),
        message(Role::User, "third"),
        message(Role::Assistant, "answer 3"),
    ];

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::Summary,
            recent_messages: 2,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context.len(), 6);
    assert_eq!(context[0].content, "FIRST important details");
    assert_eq!(context[5].content, "answer 3");
}

#[test]
fn summary_range_keeps_recent_window_raw() {
    let memory = AgentMemory {
        summarized_message_count: 1,
        ..AgentMemory::default()
    };
    let history = vec![
        message(Role::User, "1"),
        message(Role::Assistant, "2"),
        message(Role::User, "3"),
        message(Role::Assistant, "4"),
        message(Role::User, "5"),
    ];

    let range = memory
        .next_summary_range(
            &history,
            &MemoryConfig {
                strategy: MemoryStrategy::Summary,
                recent_messages: 2,
                summarize_after_messages: 4,
                summary_chunk_messages: 1,
                ..MemoryConfig::default()
            },
        )
        .expect("summary range");

    assert_eq!(range, 1..3);
}

#[test]
fn summary_storage_policy_keeps_raw_source_of_truth() {
    let memory = AgentMemory::default();
    let mut history = vec![
        message(Role::System, "System"),
        message(Role::User, "1"),
        message(Role::Assistant, "2"),
        message(Role::User, "3"),
    ];

    memory.apply_storage_policy(
        &mut history,
        &MemoryConfig {
            strategy: MemoryStrategy::Summary,
            recent_messages: 1,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(history.len(), 4);
    assert_eq!(history[1].content, "1");
}

#[test]
fn facts_update_from_key_value_and_keywords() {
    let mut memory = AgentMemory::default();

    memory.update_facts_from_user_message(
        "цель: сделать удобное управление\nНе трогай .DS_Store\nОтвечай кратко",
    );

    assert_eq!(
        memory.facts.get("goal").map(String::as_str),
        Some("сделать удобное управление")
    );
    assert!(
        memory
            .facts
            .get("constraints")
            .is_some_and(|value| value.contains("Не трогай .DS_Store"))
    );
    assert!(
        memory
            .facts
            .get("preferences")
            .is_some_and(|value| value.contains("Отвечай кратко"))
    );
}

#[test]
fn explicit_facts_overwrite_same_key_without_duplicates() {
    let mut memory = AgentMemory::default();

    memory.update_facts_from_user_message("goal: old target");
    memory.update_facts_from_user_message("goal: new target");
    memory.update_facts_from_user_message("Must keep CLI and web aligned");
    memory.update_facts_from_user_message("Must keep CLI and web aligned");

    assert_eq!(
        memory.facts.get("goal").map(String::as_str),
        Some("new target")
    );
    assert_eq!(
        memory.facts.get("constraints").map(String::as_str),
        Some("Must keep CLI and web aligned")
    );
}

#[test]
fn facts_skip_sensitive_lines() {
    let mut memory = AgentMemory::default();

    memory.update_facts_from_user_message("api key: sk-secret\nцель: безопасная память");

    assert!(!memory.facts.contains_key("api_key"));
    assert_eq!(
        memory.facts.get("goal").map(String::as_str),
        Some("безопасная память")
    );
}

#[test]
fn facts_extract_atomic_profile_and_goal_without_prompt_garbage() {
    let mut memory = AgentMemory::default();

    memory.update_facts_from_user_message("Я живу в Москве, мне 999 лет и зеленые волосы");
    memory.update_facts_from_user_message("Поэтому я хочу придумывать стихи про себя");

    assert_eq!(
        memory.facts.get("location").map(String::as_str),
        Some("Москве")
    );
    assert_eq!(memory.facts.get("age").map(String::as_str), Some("999 лет"));
    assert_eq!(
        memory.facts.get("appearance_hair").map(String::as_str),
        Some("зеленые волосы")
    );
    assert_eq!(
        memory.facts.get("goal").map(String::as_str),
        Some("придумывать стихи про себя")
    );
    assert!(!memory.facts.contains_key("preferences"));
}

#[test]
fn facts_extract_profile_and_interests_from_dialog_example() {
    let mut memory = AgentMemory::default();

    memory.update_facts_from_user_message("Привет, я 999 летний москвич с зелеными волосами");
    memory.update_facts_from_user_message("Поэтому я очень люблю стихи про себя и собак");

    assert_eq!(
        memory.facts.get("location").map(String::as_str),
        Some("Москва")
    );
    assert_eq!(memory.facts.get("age").map(String::as_str), Some("999 лет"));
    assert_eq!(
        memory.facts.get("appearance_hair").map(String::as_str),
        Some("зеленые волосы")
    );
    assert_eq!(
        memory.facts.get("interests").map(String::as_str),
        Some("стихи про себя; собаки")
    );
    assert!(!memory.facts.contains_key("preferences"));
    assert!(
        !memory
            .facts
            .values()
            .any(|value| value.contains("999 летний москвич с зелеными"))
    );
}

#[test]
fn storage_policy_keeps_complete_history_for_context_strategies() {
    let memory = AgentMemory::default();
    let mut history = vec![
        message(Role::System, "System"),
        message(Role::User, "1"),
        message(Role::Assistant, "2"),
        message(Role::User, "3"),
    ];

    memory.apply_storage_policy(
        &mut history,
        &MemoryConfig {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: 1,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(
        history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["System", "1", "2", "3"]
    );
}

#[test]
fn sticky_facts_storage_policy_keeps_complete_history() {
    let memory = AgentMemory::default();
    let mut history = vec![
        message(Role::System, "System"),
        message(Role::User, "1"),
        message(Role::Assistant, "2"),
        message(Role::User, "3"),
    ];

    memory.apply_storage_policy(
        &mut history,
        &MemoryConfig {
            strategy: MemoryStrategy::StickyFacts,
            recent_messages: 1,
            ..MemoryConfig::default()
        },
    );

    assert_eq!(
        history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["System", "1", "2", "3"]
    );
}

#[test]
fn scoped_branches_build_context_only_from_active_branch() {
    let mut memory = AgentMemory::default();
    let history = vec![
        message(Role::System, "System"),
        message(Role::User, "alpha user"),
        message(Role::Assistant, "alpha assistant"),
        message(Role::User, "beta user"),
        message(Role::Assistant, "beta assistant"),
    ];
    memory
        .branch_assignments
        .insert("1".to_string(), "alpha".to_string());
    memory
        .branch_assignments
        .insert("2".to_string(), "alpha".to_string());
    memory
        .branch_assignments
        .insert("3".to_string(), "beta".to_string());
    memory
        .branch_assignments
        .insert("4".to_string(), "beta".to_string());

    let context = memory.build_context(
        &history,
        &MemoryConfig {
            strategy: MemoryStrategy::ScopedBranches,
            recent_messages: 8,
            active_branch: "beta".to_string(),
            ..MemoryConfig::default()
        },
    );

    assert_eq!(context.len(), 3);
    assert_eq!(context[0].content, "System");
    assert_eq!(context[1].content, "beta user");
    assert_eq!(context[2].content, "beta assistant");
    assert!(
        context
            .iter()
            .all(|message| !message.content.contains("alpha"))
    );
}

#[test]
fn scoped_topic_router_returns_to_matching_existing_topic() {
    let mut memory = AgentMemory::default();
    let history = vec![
        message(Role::User, "Rust async borrow checker problem"),
        message(Role::Assistant, "Use ownership boundaries."),
        message(Role::User, "Budget planning for vacation"),
        message(Role::Assistant, "Track flights and hotels."),
    ];
    memory
        .branch_assignments
        .insert("0".to_string(), "rust async".to_string());
    memory
        .branch_assignments
        .insert("1".to_string(), "rust async".to_string());
    memory
        .branch_assignments
        .insert("2".to_string(), "vacation budget".to_string());
    memory
        .branch_assignments
        .insert("3".to_string(), "vacation budget".to_string());

    let selected = memory.select_scoped_topic(
        "Back to Rust async ownership please",
        &history,
        "vacation budget",
    );

    assert_eq!(selected, "rust async");
}

#[test]
fn scoped_topic_router_creates_new_topic_for_unrelated_message() {
    let mut memory = AgentMemory::default();
    let history = vec![
        message(Role::User, "Rust async borrow checker problem"),
        message(Role::Assistant, "Use ownership boundaries."),
    ];
    memory
        .branch_assignments
        .insert("0".to_string(), "rust async".to_string());
    memory
        .branch_assignments
        .insert("1".to_string(), "rust async".to_string());

    let selected = memory.select_scoped_topic(
        "Plan family vacation tickets and hotel budget",
        &history,
        "rust async",
    );

    assert_eq!(selected, "plan family vacation");
}

#[test]
fn scoped_topic_router_keeps_current_topic_for_short_followup() {
    let memory = AgentMemory::default();

    let selected = memory.select_scoped_topic("а дальше?", &[], "rust async");

    assert_eq!(selected, "rust async");
}

#[test]
fn scoped_branch_storage_policy_keeps_complete_history_and_assignments() {
    let mut memory = AgentMemory::default();
    let mut history = vec![
        message(Role::System, "System"),
        message(Role::User, "alpha old"),
        message(Role::Assistant, "alpha recent"),
        message(Role::User, "beta old"),
        message(Role::Assistant, "beta recent"),
    ];
    memory
        .branch_assignments
        .insert("1".to_string(), "alpha".to_string());
    memory
        .branch_assignments
        .insert("2".to_string(), "alpha".to_string());
    memory
        .branch_assignments
        .insert("3".to_string(), "beta".to_string());
    memory
        .branch_assignments
        .insert("4".to_string(), "beta".to_string());

    memory.apply_scoped_branch_storage_policy(
        &mut history,
        &MemoryConfig {
            strategy: MemoryStrategy::ScopedBranches,
            recent_messages: 1,
            active_branch: "alpha".to_string(),
            ..MemoryConfig::default()
        },
    );

    assert_eq!(
        history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "System",
            "alpha old",
            "alpha recent",
            "beta old",
            "beta recent"
        ]
    );
    assert_eq!(
        memory.branch_assignments.get("1").map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        memory.branch_assignments.get("2").map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        memory.branch_assignments.get("3").map(String::as_str),
        Some("beta")
    );
}

#[test]
fn agent_memory_toon_roundtrip_with_branch_assignments() {
    let mut memory = AgentMemory::default();
    memory
        .branch_assignments
        .insert("0".to_string(), "feature-x".to_string());
    memory
        .branch_assignments
        .insert("1".to_string(), "main".to_string());
    memory
        .branch_assignments
        .insert("2".to_string(), "feature-x".to_string());
    memory
        .facts
        .insert("topic".to_string(), "database design".to_string());
    memory.session_summary = Some("Discussed indexes and caching".to_string());
    memory.summarized_message_count = 5;

    let toon_str = crate::toon_codec::to_string(&memory).expect("encode to TOON");
    let decoded: AgentMemory = crate::toon_codec::from_str(&toon_str).expect("decode from TOON");

    assert_eq!(
        decoded.branch_assignments.get("0"),
        Some(&"feature-x".to_string())
    );
    assert_eq!(
        decoded.branch_assignments.get("1"),
        Some(&"main".to_string())
    );
    assert_eq!(
        decoded.branch_assignments.get("2"),
        Some(&"feature-x".to_string())
    );
    assert_eq!(
        decoded.facts.get("topic"),
        Some(&"database design".to_string())
    );
    assert_eq!(
        decoded.session_summary,
        Some("Discussed indexes and caching".to_string())
    );
    assert_eq!(decoded.summarized_message_count, 5);
}

#[test]
fn agent_memory_json_fallback_with_branch_assignments() {
    let json = r#"{"facts":{},"branch_assignments":{"0":"feature-x","1":"main","2":"feature-x"},"session_summary":"Old session","summarized_message_count":3}"#;
    let decoded: AgentMemory = crate::toon_codec::from_str_or_json(json).expect("decode from JSON");

    assert_eq!(
        decoded.branch_assignments.get("0"),
        Some(&"feature-x".to_string())
    );
    assert_eq!(
        decoded.branch_assignments.get("1"),
        Some(&"main".to_string())
    );
    assert_eq!(
        decoded.branch_assignments.get("2"),
        Some(&"feature-x".to_string())
    );
    assert_eq!(decoded.session_summary, Some("Old session".to_string()));
    assert_eq!(decoded.summarized_message_count, 3);
}
