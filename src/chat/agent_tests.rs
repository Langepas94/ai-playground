use super::*;
use async_trait::async_trait;

#[test]
fn parse_extracted_facts_reads_agent_chosen_layer() {
    // Preferred shape: explicit per-fact layer.
    let facts = parse_extracted_facts(
        r#"{"facts":[{"key":"goal","value":"ship","layer":"working"},{"key":"prefs","value":"concise","layer":"long-term"}]}"#,
    )
    .expect("parsed");
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].key, "goal");
    assert_eq!(facts[0].layer, Some(MemoryLayer::Working));
    assert_eq!(facts[1].layer, Some(MemoryLayer::LongTerm));

    // Legacy flat object still parses with no layer (→ default routing later).
    let legacy = parse_extracted_facts(r#"{"favorite_color":"green","interests":"dogs"}"#)
        .expect("legacy parsed");
    assert_eq!(legacy.len(), 2);
    assert!(legacy.iter().all(|fact| fact.layer.is_none()));
}

#[derive(Debug, Default)]
struct FakeClient {
    replies: std::sync::Mutex<Vec<String>>,
    metrics: std::sync::Mutex<Vec<crate::providers::RequestMetrics>>,
    seen_messages: std::sync::Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait]
impl ProviderClient for FakeClient {
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
        self.seen_messages
            .lock()
            .expect("seen messages")
            .push(request.messages);
        let text = self
            .replies
            .lock()
            .expect("replies")
            .pop()
            .unwrap_or_else(|| "ok".to_string());
        let metrics = self.metrics.lock().expect("metrics").pop().unwrap_or(
            crate::providers::RequestMetrics {
                elapsed_ms: 1,
                usage: None,
                cost: None,
            },
        );
        Ok(ChatResponse {
            text,
            finish_reason: Some("stop".to_string()),
            metrics,
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

fn test_profile() -> ProfileConfig {
    ProfileConfig {
        provider: crate::providers::ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: "https://example.test/v1".to_string(),
        token_ref: "openai-compatible".to_string(),
    }
}

#[tokio::test]
async fn agent_injects_working_and_long_term_blocks() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["ok".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::SlidingWindow,
        recent_messages: 4,
        ..MemoryConfig::default()
    });
    agent.set_task_state(Some(crate::chat::store::TaskContext {
        stage: crate::chat::store::TaskStage::Planning,
        title: "ship agents".to_string(),
        goal: "persist settings".to_string(),
        ..crate::chat::store::TaskContext::default()
    }));
    agent.set_agent_profile(Some(crate::chat::store::AgentProfile {
        fields: vec![crate::chat::store::ProfileField {
            key: "stack".to_string(),
            question: "Which stack?".to_string(),
            required: true,
            value: "Rust".to_string(),
        }],
        updated_at_unix: 0,
    }));
    agent.set_invariants(vec!["Only Rust".to_string()]);
    agent.set_domain("rust backend assistant");

    agent
        .respond(&client, "hi".to_string())
        .await
        .expect("respond");

    let seen = client.seen_messages.lock().unwrap();
    let joined = seen[0]
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("[memory:working]"), "working block missing");
    assert!(joined.contains("Stage: planning"));
    assert!(joined.contains("ship agents"));
    assert!(
        joined.contains("[memory:long-term]"),
        "long-term block missing"
    );
    assert!(joined.contains("stack: Rust"));
    assert!(joined.contains("[invariants]"), "invariants block missing");
    assert!(joined.contains("Only Rust"));
    assert!(joined.contains("[agent:domain]"), "domain block missing");
    assert!(
        joined.contains("do not treat it as a refusal policy"),
        "domain should not become a hard refusal boundary"
    );
}

#[tokio::test]
async fn runtime_user_profile_is_separate_payload_block() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["ok".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_domain("coding-agent");
    agent.set_user_profile(Some(crate::chat::store::UserProfile {
        id: "artem-short-russian".to_string(),
        display_name: "Artem short Russian".to_string(),
        style_preferences: vec!["be concise".to_string()],
        language_preferences: vec!["Russian".to_string()],
        response_length: "short".to_string(),
        custom_instructions: "avoid tables unless requested".to_string(),
        ..crate::chat::store::UserProfile::default()
    }));

    agent
        .respond(&client, "answer in detail this time".to_string())
        .await
        .expect("respond");

    let seen = client.seen_messages.lock().unwrap();
    let messages = &seen[0];
    let joined = messages
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("[agent:domain]"), "agent block missing");
    assert!(
        joined.contains("[user-profile]"),
        "runtime user profile block missing"
    );
    assert!(joined.contains("be concise"));
    assert!(joined.contains("Russian"));
    assert!(
        joined.contains("Do not change the agent identity, tools, workflow, or capabilities"),
        "profile must not alter agent capabilities"
    );
    assert!(
        joined.contains("current user request explicitly conflicts with this profile"),
        "profile block must document user override precedence"
    );
    assert_eq!(
        messages.last().expect("last").content,
        "answer in detail this time"
    );
}

#[tokio::test]
async fn same_agent_can_use_different_user_profiles_at_runtime() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["two".to_string(), "one".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let make_profile = |id: &str, style: &str| crate::chat::store::UserProfile {
        id: id.to_string(),
        display_name: id.to_string(),
        style_preferences: vec![style.to_string()],
        ..crate::chat::store::UserProfile::default()
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_domain("support-agent");
    agent.set_user_profile(Some(make_profile("manager-detailed", "give details")));
    agent
        .respond(&client, "status?".to_string())
        .await
        .expect("first");
    agent.set_user_profile(Some(make_profile("beginner-friendly", "explain gently")));
    agent
        .respond(&client, "status?".to_string())
        .await
        .expect("second");

    let seen = client.seen_messages.lock().unwrap();
    let first = seen[0]
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let second = seen[1]
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first.contains("give details"));
    assert!(!first.contains("explain gently"));
    assert!(second.contains("explain gently"));
    assert!(second.contains("[agent:domain] Specialization/background: support-agent"));
}

#[test]
fn task_block_keeps_tracked_task_as_context_not_a_gate() {
    let block = render_task_block(&crate::chat::store::TaskContext {
        stage: crate::chat::store::TaskStage::Clarify,
        title: "name pizzeria".to_string(),
        goal: "invent a pizzeria name".to_string(),
        ..crate::chat::store::TaskContext::default()
    });

    assert!(block.contains("[memory:working]"));
    assert!(block.contains("Stage: clarify"));
    assert!(block.contains("background for the tracked task"));
    assert!(block.contains("Do not force the user to finish this task"));
    assert!(
        !block.contains("Do NOT skip stages"),
        "working memory must not make unfinished tracked tasks block adjacent dialogs"
    );
}

#[tokio::test]
async fn stateful_postprocess_seeds_task_goal_from_first_prompt() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            r#"{"stage":"clarify","reason":"asking clarifying questions"}"#.to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(crate::chat::store::TaskContext::default()));

    agent
        .stateful_postprocess(
            &client,
            "Давай придумаем продуктовое меню для пиццерии. Команда небольшая.",
            "Сначала уточню детали.",
        )
        .await;

    let task = agent.task_state().expect("task state");
    assert_eq!(
        task.goal,
        "Давай придумаем продуктовое меню для пиццерии. Команда небольшая."
    );
    assert_eq!(task.title, "Давай придумаем продуктовое меню для пиццерии");
    assert_eq!(task.stage, crate::chat::store::TaskStage::Clarify);
}

#[tokio::test]
async fn stateful_postprocess_captures_approved_task_decision() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            r#"{"stage":"clarify","reason":"decision captured"}"#.to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_task_state(Some(crate::chat::store::TaskContext {
        goal: "Придумать название пиццерии".to_string(),
        ..crate::chat::store::TaskContext::default()
    }));

    agent
        .stateful_postprocess(
            &client,
            "ВолгаПицца самое то, утверждаем",
            "Отлично, «ВолгаПицца» — выбор сделан!",
        )
        .await;

    let task = agent.task_state().expect("task state");
    assert!(
        task.results.iter().any(|item| item.contains("ВолгаПицца")),
        "approved brand name should become shared working-task result"
    );
}

#[tokio::test]
async fn sliding_window_sends_only_recent_messages_and_keeps_history() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = (0..6)
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: format!("history {index}"),
        })
        .collect::<Vec<_>>();
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::SlidingWindow,
        recent_messages: 2,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "current question".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].len(), 3);
    assert_eq!(seen[0][0].content, "history 4");
    assert_eq!(seen[0][1].content, "history 5");
    assert_eq!(seen[0][2].content, "current question");
    assert_eq!(
        agent.history().len(),
        8,
        "Sliding Window must only limit provider context; local/UI history stays complete"
    );
    assert_eq!(agent.history()[0].content, "history 0");
    assert_eq!(agent.history()[5].content, "history 5");
    assert_eq!(agent.history()[6].content, "current question");
    assert_eq!(agent.history()[7].content, "answer");
}

#[tokio::test]
async fn sticky_facts_updates_before_request_and_sends_facts_block() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        vec![ChatMessage {
            role: Role::System,
            content: "Base system".to_string(),
        }],
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 4,
        facts_extraction_prompt: String::new(),
        ..MemoryConfig::default()
    });

    agent
        .respond(
            &client,
            "цель: реализовать context strategies\nОтвечай кратко".to_string(),
        )
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen[0].len(), 3);
    assert_eq!(seen[0][0].role, Role::System);
    assert_eq!(seen[0][0].content, "Base system");
    assert_eq!(seen[0][1].role, Role::System);
    assert!(seen[0][1].content.contains("FACTS_KV:"));
    assert!(
        seen[0][1]
            .content
            .contains("goal: реализовать context strategies")
    );
    assert!(seen[0][1].content.contains("preferences: Отвечай кратко"));
    assert!(agent.memory().facts.contains_key("goal"));
}

#[tokio::test]
async fn saved_long_term_facts_are_sent_with_default_strategy() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut memory = AgentMemory::default();
    memory.set_fact_in_layer(
        "project_context".to_string(),
        "Пиццерия находится в Волгограде; город должен фигурировать в названии.".to_string(),
        MemoryLayer::LongTerm,
    );
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::SlidingWindow,
        recent_messages: 4,
        ..MemoryConfig::default()
    });

    agent
        .respond(
            &client,
            "Помоги придумать название, город обязательно должен фигурировать.".to_string(),
        )
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    let joined = seen[0]
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("FACTS_KV:"),
        "persisted facts block must be sent even outside Sticky Facts"
    );
    assert!(joined.contains("[long-term]"));
    assert!(joined.contains("project_context"));
    assert!(joined.contains("Волгограде"));
}

#[tokio::test]
async fn sticky_facts_uses_facts_plus_window_without_extra_provider_call() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = (0..6)
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: format!("history {index}"),
        })
        .collect::<Vec<_>>();
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 2,
        facts_extraction_prompt: String::new(),
        ..MemoryConfig::default()
    });

    agent
        .respond(
            &client,
            "goal: keep durable facts\napi key: sk-should-not-leak".to_string(),
        )
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(
        seen.len(),
        1,
        "blank facts extraction prompt keeps the local fallback path"
    );
    let request = &seen[0];
    assert_eq!(
        request.len(),
        3,
        "provider request must be facts + exactly last N raw messages including current prompt"
    );
    assert_eq!(request[0].role, Role::System);
    assert!(request[0].content.contains("FACTS_KV:"));
    assert!(request[0].content.contains("goal: keep durable facts"));
    assert!(!request[0].content.contains("sk-should-not-leak"));
    assert_eq!(request[1].content, "history 5");
    assert_eq!(
        request[2].content,
        "goal: keep durable facts\napi key: sk-should-not-leak"
    );
    assert!(
        request
            .iter()
            .all(|message| !message.content.contains("history 0"))
    );
    assert_eq!(
        agent.history().len(),
        8,
        "Sticky Facts must keep full local conversation history; recent_messages only limits provider request"
    );
    assert_eq!(agent.history()[0].content, "history 0");
    assert_eq!(agent.history()[5].content, "history 5");
    assert_eq!(
        agent.history()[6].content,
        "goal: keep durable facts\napi key: sk-should-not-leak"
    );
    assert_eq!(agent.history()[7].content, "answer");
}

#[tokio::test]
async fn sticky_facts_update_after_every_user_message_and_request_sends_facts_plus_last_n() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "second answer".to_string(),
            "first answer".to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 3,
        facts_extraction_prompt: String::new(),
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "goal: build reliable facts memory".to_string())
        .await
        .expect("first response");
    agent
        .respond(
            &client,
            "constraint: show persisted KV and provider block".to_string(),
        )
        .await
        .expect("second response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0][0]
            .content
            .contains("goal: build reliable facts memory")
    );
    assert_eq!(
        seen[0]
            .iter()
            .filter(|message| message.role != Role::System)
            .count(),
        1,
        "first request has facts plus the current user message"
    );

    let second_request = &seen[1];
    let facts = &second_request[0].content;
    assert!(facts.contains("FACTS_KV:"));
    assert!(facts.contains("- goal: build reliable facts memory"));
    assert!(facts.contains("- constraints: show persisted KV and provider block"));
    let raw_messages = second_request
        .iter()
        .filter(|message| message.role != Role::System)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        raw_messages,
        vec![
            "goal: build reliable facts memory",
            "first answer",
            "constraint: show persisted KV and provider block"
        ],
        "recent_messages=3 means the provider sees exactly the last 3 raw messages including current user prompt"
    );
    assert_eq!(
        agent.memory().facts.get("goal").map(String::as_str),
        Some("build reliable facts memory")
    );
    assert_eq!(
        agent.memory().facts.get("constraints").map(String::as_str),
        Some("show persisted KV and provider block")
    );
    assert_eq!(
        agent
            .history()
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "goal: build reliable facts memory",
            "first answer",
            "constraint: show persisted KV and provider block",
            "second answer"
        ],
        "Sticky Facts keeps full saved history while request uses only a recent window"
    );
}

#[tokio::test]
async fn sticky_facts_sends_custom_facts_prompt_to_provider() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 2,
        facts_extraction_prompt: String::new(),
        facts_prompt: "Use these project facts when answering.".to_string(),
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "goal: expose custom facts prompt".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0][0]
            .content
            .starts_with("Use these project facts when answering.")
    );
    assert!(
        seen[0][0]
            .content
            .contains("goal: expose custom facts prompt")
    );
}

#[tokio::test]
async fn sticky_facts_custom_extraction_prompt_controls_saved_kv() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "main answer".to_string(),
            r#"{"favorite_color":"зеленый","interests":"собаки"}"#.to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::StickyFacts,
        recent_messages: 2,
        facts_extraction_prompt: "Collect only favorite colors and interests. Return JSON."
            .to_string(),
        ..MemoryConfig::default()
    });

    agent
        .respond(
            &client,
            "Мой любимый цвет зеленый, и я люблю собак".to_string(),
        )
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(
        seen.len(),
        2,
        "Sticky Facts with extraction prompt must call extractor before the main request"
    );
    assert_eq!(seen[0][0].role, Role::System);
    assert!(seen[0][0].content.contains("favorite colors"));
    assert!(seen[0][1].content.contains("Мой любимый цвет зеленый"));
    assert_eq!(
        agent
            .memory()
            .facts
            .get("favorite_color")
            .map(String::as_str),
        Some("зеленый")
    );
    assert_eq!(
        agent.memory().facts.get("interests").map(String::as_str),
        Some("собаки")
    );
    assert!(seen[1][0].content.contains("FACTS_KV:"));
    assert!(seen[1][0].content.contains("- favorite_color: зеленый"));
    assert!(seen[1][0].content.contains("- interests: собаки"));
    assert_eq!(
        seen[1].last().map(|message| message.content.as_str()),
        Some("Мой любимый цвет зеленый, и я люблю собак")
    );
}

#[tokio::test]
async fn summary_strategy_compacts_old_history_after_response() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "summary of older turns".to_string(),
            "fresh answer".to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = (0..20)
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: format!("history {index}"),
        })
        .collect::<Vec<_>>();
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::Summary,
        recent_messages: 4,
        summarize_after_messages: 18,
        summary_chunk_messages: 4,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "current question".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].len(),
        21,
        "first request must not drop unsummarized history"
    );
    assert_eq!(seen[0][0].content, "history 0");
    assert_eq!(seen[0][20].content, "current question");
    assert_eq!(seen[1][0].role, Role::System);
    assert!(seen[1][0].content.contains("memory compaction module"));
    assert!(seen[1][1].content.contains("history 0"));
    assert_eq!(
        agent.memory().session_summary.as_deref(),
        Some("summary of older turns")
    );
    assert_eq!(agent.history().len(), 22);
}

#[tokio::test]
async fn summary_strategy_sends_custom_summary_prompt_to_provider() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "custom summary".to_string(),
            "fresh answer".to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = (0..8)
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: format!("history {index}"),
        })
        .collect::<Vec<_>>();
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::Summary,
        recent_messages: 2,
        summarize_after_messages: 4,
        summary_chunk_messages: 2,
        summary_prompt: "CUSTOM SUMMARY PROMPT".to_string(),
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "current question".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1][0].content, "CUSTOM SUMMARY PROMPT");
    assert!(seen[1][1].content.contains("Previous memory summary"));
    assert!(seen[1][1].content.contains("history 0"));
}

#[test]
fn agent_estimates_next_exchange_from_current_history() {
    let agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        vec![
            ChatMessage {
                role: Role::System,
                content: "Ты эксперт по Civilization 6.".to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: "Как играть за Византию?".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "Через религию и кавалерию.".to_string(),
            },
        ],
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        Some(ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(1.0),
            output_per_million: 4.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        }),
        None,
    );

    let estimate = agent.estimate_next_exchange(
        "Что делать, если я отстал по науке?",
        "Сократи религиозные вложения и подними кампусы.",
        512,
    );

    assert!(estimate.current_request_tokens > estimate.history_tokens);
    assert!(estimate.response_tokens > 0);
    assert!(estimate.cost.expect("cost").amount > 0.0);
}

#[tokio::test]
async fn branching_strategy_uses_current_branch_history_only() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["branch answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = vec![
        ChatMessage {
            role: Role::User,
            content: "checkpoint question".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "checkpoint answer".to_string(),
        },
        ChatMessage {
            role: Role::User,
            content: "branch A turn".to_string(),
        },
    ];
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::Branching,
        recent_messages: 8,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "continue branch A".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .iter()
            .any(|message| message.content.contains("branch A turn"))
    );
    assert!(
        seen[0]
            .iter()
            .all(|message| !message.content.contains("branch B"))
    );
}

#[tokio::test]
async fn branching_keeps_two_branches_independent_from_same_checkpoint() {
    let checkpoint = vec![
        ChatMessage {
            role: Role::User,
            content: "checkpoint question".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "checkpoint answer".to_string(),
        },
    ];
    let branch_a_history = checkpoint
        .iter()
        .cloned()
        .chain(std::iter::once(ChatMessage {
            role: Role::User,
            content: "branch A only".to_string(),
        }))
        .collect::<Vec<_>>();
    let branch_b_history = checkpoint
        .iter()
        .cloned()
        .chain(std::iter::once(ChatMessage {
            role: Role::User,
            content: "branch B only".to_string(),
        }))
        .collect::<Vec<_>>();
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "branch B answer".to_string(),
            "branch A answer".to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let config = MemoryConfig {
        strategy: MemoryStrategy::Branching,
        recent_messages: 8,
        ..MemoryConfig::default()
    };
    let mut branch_a = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        branch_a_history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    branch_a.set_memory_config(config.clone());
    let mut branch_b = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        branch_b_history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    branch_b.set_memory_config(config);

    branch_a
        .respond(&client, "continue A".to_string())
        .await
        .expect("branch A response");
    branch_b
        .respond(&client, "continue B".to_string())
        .await
        .expect("branch B response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0]
            .iter()
            .any(|message| message.content == "branch A only")
    );
    assert!(
        seen[0]
            .iter()
            .all(|message| message.content != "branch B only")
    );
    assert!(
        seen[1]
            .iter()
            .any(|message| message.content == "branch B only")
    );
    assert!(
        seen[1]
            .iter()
            .all(|message| message.content != "branch A only")
    );
}

#[tokio::test]
async fn scoped_branches_keep_one_session_but_filter_provider_context() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["beta answer".to_string(), "alpha answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        recent_messages: 8,
        active_branch: "alpha".to_string(),
        scoped_auto_route: false,
        ..MemoryConfig::default()
    });
    agent
        .respond(&client, "alpha question".to_string())
        .await
        .expect("alpha response");

    let mut beta_config = agent.memory_config();
    beta_config.active_branch = "beta".to_string();
    agent.set_memory_config(beta_config);
    agent
        .respond(&client, "beta question".to_string())
        .await
        .expect("beta response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0]
            .iter()
            .any(|message| message.content == "alpha question")
    );
    assert!(
        seen[1]
            .iter()
            .any(|message| message.content == "beta question")
    );
    assert!(
        seen[1]
            .iter()
            .all(|message| !message.content.contains("alpha"))
    );
    assert!(
        agent
            .history()
            .iter()
            .any(|message| message.content == "alpha question")
    );
    assert!(
        agent
            .history()
            .iter()
            .any(|message| message.content == "beta question")
    );
    assert_eq!(
        agent
            .memory()
            .branch_assignments
            .get("0")
            .map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        agent
            .memory()
            .branch_assignments
            .get("2")
            .map(String::as_str),
        Some("beta")
    );
}

#[tokio::test]
async fn scoped_branches_auto_route_back_to_existing_topic() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = vec![
        ChatMessage {
            role: Role::User,
            content: "Rust async borrow checker problem".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "Use ownership boundaries.".to_string(),
        },
        ChatMessage {
            role: Role::User,
            content: "Vacation budget and hotel plan".to_string(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "Track flights and hotels.".to_string(),
        },
    ];
    let mut memory = AgentMemory::default();
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
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        recent_messages: 8,
        active_branch: "vacation budget".to_string(),
        scoped_auto_route: true,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "Back to Rust async ownership".to_string())
        .await
        .expect("response");

    assert_eq!(agent.memory_config().active_branch, "rust async");
    let seen = client.seen_messages.lock().expect("seen messages");
    assert!(
        seen[0]
            .iter()
            .any(|message| message.content.contains("Rust async"))
    );
    assert!(
        seen[0]
            .iter()
            .all(|message| !message.content.contains("Vacation budget"))
    );
}

#[tokio::test]
async fn topic_file_routing_classifies_with_metadata_and_loads_only_selected_topic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = crate::chat::LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("session");
    let topic_store = store.topic_file_storage(&session.id).expect("topic store");
    let rust_topic = crate::chat::memory::TopicFile {
        metadata: crate::chat::memory::TopicMetadata {
            id: "rust".to_string(),
            title: "Rust".to_string(),
            short_description: "Rust async ownership work.".to_string(),
            tags: vec!["rust".to_string(), "async".to_string()],
            message_count: 2,
            updated_at_unix: 1,
        },
        context: "RUST TOPIC CONTEXT".to_string(),
    };
    let travel_topic = crate::chat::memory::TopicFile {
        metadata: crate::chat::memory::TopicMetadata {
            id: "travel".to_string(),
            title: "Travel".to_string(),
            short_description: "Vacation planning.".to_string(),
            tags: vec!["hotel".to_string(), "budget".to_string()],
            message_count: 2,
            updated_at_unix: 1,
        },
        context: "TRAVEL TOPIC CONTEXT".to_string(),
    };
    topic_store
        .save_topic_file(&rust_topic)
        .expect("save rust topic");
    topic_store
        .save_topic_file(&travel_topic)
        .expect("save travel topic");
    let mut memory = AgentMemory::default();
    memory
        .topic_catalog
        .insert("rust".to_string(), rust_topic.metadata.clone());
    memory
        .topic_catalog
        .insert("travel".to_string(), travel_topic.metadata.clone());
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "main answer".to_string(),
            r#"{"found":true,"topic_id":"rust","confidence":0.94,"reason":"matches rust async"}"#
                .to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_topic_store(Some(topic_store.clone()));
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        topic_file_routing: true,
        topic_auto_create: false,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "Rust async followup".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 2);
    let classifier_payload = seen[0]
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(classifier_payload.contains("Rust async ownership work."));
    assert!(classifier_payload.contains("Vacation planning."));
    assert!(!classifier_payload.contains("RUST TOPIC CONTEXT"));
    assert!(!classifier_payload.contains("TRAVEL TOPIC CONTEXT"));
    let provider_payload = seen[1]
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(provider_payload.contains("RUST TOPIC CONTEXT"));
    assert!(!provider_payload.contains("TRAVEL TOPIC CONTEXT"));
    assert_eq!(agent.memory_config().active_branch, "rust");
    let updated_rust = topic_store
        .load_topic_file("rust")
        .expect("load rust topic")
        .expect("rust topic");
    let unchanged_travel = topic_store
        .load_topic_file("travel")
        .expect("load travel topic")
        .expect("travel topic");
    assert!(updated_rust.context.contains("Rust async followup"));
    assert_eq!(unchanged_travel.context, "TRAVEL TOPIC CONTEXT");
}

#[tokio::test]
async fn topic_file_routing_not_found_skips_main_provider_request() {
    let mut memory = AgentMemory::default();
    memory.topic_catalog.insert(
        "rust".to_string(),
        crate::chat::memory::TopicMetadata {
            id: "rust".to_string(),
            title: "Rust".to_string(),
            short_description: "Rust async ownership work.".to_string(),
            tags: vec!["rust".to_string()],
            message_count: 2,
            updated_at_unix: 1,
        },
    );
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            r#"{"found":false,"topic_id":null,"confidence":0.12,"reason":"no matching topic"}"#
                .to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        topic_file_routing: true,
        topic_auto_create: false,
        ..MemoryConfig::default()
    });

    let response = agent
        .respond(&client, "Plan a vacation".to_string())
        .await
        .expect("response");

    assert_eq!(response.finish_reason.as_deref(), Some("topic_not_found"));
    assert!(response.text.contains("Не нашёл подходящий topic-файл"));
    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen.len(), 1, "only classifier request should be sent");
}

#[tokio::test]
async fn topic_file_routing_switches_topic_on_classifier_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = crate::chat::LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("session");
    let topic_store = store.topic_file_storage(&session.id).expect("topic store");
    for (id, context) in [("rust", "RUST CONTEXT"), ("travel", "TRAVEL CONTEXT")] {
        let topic = crate::chat::memory::TopicFile {
            metadata: crate::chat::memory::TopicMetadata {
                id: id.to_string(),
                title: id.to_string(),
                short_description: format!("{id} topic"),
                tags: vec![id.to_string()],
                message_count: 2,
                updated_at_unix: 1,
            },
            context: context.to_string(),
        };
        topic_store.save_topic_file(&topic).expect("save topic");
    }
    let mut memory = AgentMemory::default();
    for id in ["rust", "travel"] {
        memory.topic_catalog.insert(
            id.to_string(),
            crate::chat::memory::TopicMetadata {
                id: id.to_string(),
                title: id.to_string(),
                short_description: format!("{id} topic"),
                tags: vec![id.to_string()],
                message_count: 2,
                updated_at_unix: 1,
            },
        );
    }
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "travel answer".to_string(),
            r#"{"found":true,"topic_id":"travel","confidence":0.91,"reason":"travel drift"}"#
                .to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_topic_store(Some(topic_store));
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        active_branch: "rust".to_string(),
        topic_file_routing: true,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "Hotel budget followup".to_string())
        .await
        .expect("response");

    assert_eq!(agent.memory_config().active_branch, "travel");
    let seen = client.seen_messages.lock().expect("seen messages");
    let provider_payload = seen[1]
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(provider_payload.contains("TRAVEL CONTEXT"));
    assert!(!provider_payload.contains("RUST CONTEXT"));
}

#[tokio::test]
async fn topic_file_routing_does_not_persist_sensitive_turns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = crate::chat::LocalSessionStore::from_root(dir.path().join("sessions"));
    let session = store.create_session().expect("session");
    let topic_store = store.topic_file_storage(&session.id).expect("topic store");
    let topic = crate::chat::memory::TopicFile {
        metadata: crate::chat::memory::TopicMetadata {
            id: "rust".to_string(),
            title: "Rust".to_string(),
            short_description: "Rust work.".to_string(),
            tags: vec!["rust".to_string()],
            message_count: 2,
            updated_at_unix: 1,
        },
        context: "SAFE CONTEXT".to_string(),
    };
    topic_store.save_topic_file(&topic).expect("save topic");
    let mut memory = AgentMemory::default();
    memory
        .topic_catalog
        .insert("rust".to_string(), topic.metadata.clone());
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec![
            "main answer".to_string(),
            r#"{"found":true,"topic_id":"rust","confidence":0.94,"reason":"rust"}"#.to_string(),
        ]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        memory,
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_topic_store(Some(topic_store.clone()));
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        topic_file_routing: true,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "my token is sk-secret".to_string())
        .await
        .expect("response");

    let loaded = topic_store
        .load_topic_file("rust")
        .expect("load topic")
        .expect("topic");
    assert_eq!(loaded.context, "SAFE CONTEXT");
}

#[tokio::test]
async fn scoped_branches_allow_manual_topic_when_auto_route_is_off() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let history = vec![ChatMessage {
        role: Role::User,
        content: "Rust async borrow checker problem".to_string(),
    }];
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::ScopedBranches,
        recent_messages: 8,
        active_branch: "manual finance".to_string(),
        scoped_auto_route: false,
        ..MemoryConfig::default()
    });

    agent
        .respond(&client, "Rust async ownership followup".to_string())
        .await
        .expect("response");

    assert_eq!(agent.memory_config().active_branch, "manual finance");
    assert_eq!(
        agent
            .memory()
            .branch_assignments
            .get("1")
            .map(String::as_str),
        Some("manual finance")
    );
}

#[tokio::test]
async fn agent_uses_custom_system_prompt_without_default_agent_prompt() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(Vec::new()),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        vec![ChatMessage {
            role: Role::System,
            content: "Ты эксперт по Civilization 6.".to_string(),
        }],
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );

    agent
        .respond(&client, "Как играть за Византию?".to_string())
        .await
        .expect("response");

    let seen = client.seen_messages.lock().expect("seen messages");
    assert_eq!(seen[0].len(), 2);
    assert_eq!(seen[0][0].role, Role::System);
    assert_eq!(seen[0][0].content, "Ты эксперт по Civilization 6.");
    assert_eq!(seen[0][1].content, "Как играть за Византию?");
}

#[test]
fn agent_registry_selects_known_agent() {
    let agent = selected_agent(Some(LOCAL_SESSION_AGENT_ID)).expect("agent");

    assert_eq!(agent.id, LOCAL_SESSION_AGENT_ID);
    assert!(!available_agents().is_empty());
    assert!(matches!(
        selected_agent(Some("missing-agent")),
        Err(AppError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn agent_gracefully_degrades_when_context_overflows() {
    let client = FakeClient {
        replies: std::sync::Mutex::new(vec!["answer".to_string()]),
        metrics: std::sync::Mutex::new(vec![crate::providers::RequestMetrics {
            elapsed_ms: 1,
            usage: None,
            cost: None,
        }]),
        seen_messages: std::sync::Mutex::new(Vec::new()),
    };

    let history = (0..50)
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: "x".repeat(500),
        })
        .collect::<Vec<_>>();

    let original_history_len = history.len();

    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        history,
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );
    agent.set_context_limit(Some(512));
    agent.set_memory_config(MemoryConfig {
        strategy: MemoryStrategy::SlidingWindow,
        recent_messages: 12,
        ..MemoryConfig::default()
    });

    let response = agent
        .respond(&client, "new question".to_string())
        .await
        .expect("response");

    assert!(!response.text.is_empty(), "response should be generated");

    let seen = client.seen_messages.lock().expect("seen");
    assert!(!seen.is_empty(), "should have sent at least one request");

    let main_request = &seen[seen.len() - 1];
    assert!(
        main_request.len() < original_history_len + 1,
        "context window strategy should limit messages sent (not send all 50+ messages). sent {} messages out of {}",
        main_request.len(),
        original_history_len
    );
}

#[test]
fn agent_context_limit_can_be_updated() {
    let mut agent = ChatAgent::new(
        test_profile(),
        "secret".to_string(),
        Vec::new(),
        AgentMemory::default(),
        ResponseControl::uncontrolled(),
        None,
        None,
    );

    assert!(agent.context_limit().is_none());
    agent.set_context_limit(Some(8192));
    assert_eq!(agent.context_limit(), Some(8192));
}
