
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
        ("unknown", MemoryStrategy::SlidingWindow),
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
