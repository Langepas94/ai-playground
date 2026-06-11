use ai_playground::{
    config::ProfileConfig,
    errors::{AppError, HttpProblem},
    providers::{
        ChatMessage, ChatRequest, CostSource, ModelPricing, ProviderClient, ProviderKind,
        ReqwestProviderClient, ResponseControl, Role,
    },
};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{bearer_token, body_string_contains, header, header_exists, method, path},
};

#[tokio::test]
async fn chat_completion_uses_mock_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "hello back" } }]
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: server.uri(),
        token_ref: "openai-compatible:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let response = client
        .chat_completion(
            &profile,
            "secret",
            ChatRequest {
                model: "test-model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect("chat");

    assert_eq!(response.text, "hello back");
}

#[tokio::test]
async fn chat_completion_calculates_cost_from_configured_pricing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "hello back" } }],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "total_tokens": 1500
            }
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: server.uri(),
        token_ref: "openai-compatible:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let response = client
        .chat_completion(
            &profile,
            "secret",
            ChatRequest {
                model: "test-model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: Some(ModelPricing {
                    currency: "USD".to_string(),
                    input_per_million: Some(2.0),
                    output_per_million: 10.0,
                    cache_hit_input_per_million: None,
                    cache_miss_input_per_million: None,
                }),
                billing: None,
            },
        )
        .await
        .expect("chat");

    let cost = response.metrics.cost.expect("configured cost");
    assert!((cost.amount - 0.007).abs() < f64::EPSILON);
    assert_eq!(cost.currency, "USD");
    assert_eq!(cost.source, CostSource::ConfiguredPricing);
}

#[tokio::test]
async fn gigachat_refreshes_access_token_after_unauthorized_and_counts_usage() {
    let api_server = MockServer::start().await;
    let oauth_server = MockServer::start().await;
    let oauth_calls = Arc::new(AtomicUsize::new(0));
    let oauth_calls_for_mock = oauth_calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/v2/oauth"))
        .and(header("authorization", "Basic auth-key"))
        .and(header_exists("rquid"))
        .and(body_string_contains("scope=GIGACHAT_API_PERS"))
        .respond_with(move |_request: &Request| {
            let call = oauth_calls_for_mock.fetch_add(1, Ordering::SeqCst);
            let access_token = if call == 0 {
                "expired.access.token"
            } else {
                "fresh.access.token"
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "access_token": access_token,
                "expires_at": 4_000_000_000_000_u64
            }))
        })
        .expect(2)
        .mount(&oauth_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("expired.access.token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "message": "token expired"
        })))
        .expect(1)
        .mount(&api_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("fresh.access.token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "привет" }, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 7,
                "total_tokens": 19
            }
        })))
        .expect(1)
        .mount(&api_server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::GigaChat,
        model: "GigaChat".to_string(),
        base_url: api_server.uri(),
        token_ref: "gigachat:test".to_string(),
    };
    let client = ReqwestProviderClient::new_with_gigachat_oauth_url(format!(
        "{}/api/v2/oauth",
        oauth_server.uri()
    ))
    .expect("client");
    let response = client
        .chat_completion(
            &profile,
            "auth-key",
            ChatRequest {
                model: "GigaChat".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "Привет".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect("chat");

    assert_eq!(oauth_calls.load(Ordering::SeqCst), 2);
    assert_eq!(response.text, "привет");
    let usage = response.metrics.usage.expect("usage");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.total_tokens, 19);
}

#[tokio::test]
async fn chat_completion_debug_redacts_auth_and_keeps_json_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "hello back" }, "finish_reason": "stop" }]
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: server.uri(),
        token_ref: "openai-compatible:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let (response, debug) = client
        .chat_completion_with_debug(
            &profile,
            "secret",
            ChatRequest {
                model: "test-model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect("chat");

    assert_eq!(response.text, "hello back");
    assert_eq!(debug.request.headers["authorization"], "Bearer [redacted]");
    assert_eq!(debug.request.body["model"], "test-model");
    assert_eq!(debug.response.status, 200);
    assert_eq!(
        debug.response.body["choices"][0]["message"]["content"],
        "hello back"
    );
}

#[tokio::test]
async fn stream_chat_completion_debug_keeps_metrics_cost_and_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"back\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":500,\"total_tokens\":1500}}\n\n",
            "data: [DONE]\n\n"
        )))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: server.uri(),
        token_ref: "openai-compatible:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let streamed_chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let chunks_for_callback = streamed_chunks.clone();
    let (response, debug) = client
        .stream_chat_completion_with_debug(
            &profile,
            "secret",
            ChatRequest {
                model: "test-model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: Some(ModelPricing {
                    currency: "USD".to_string(),
                    input_per_million: Some(2.0),
                    output_per_million: 10.0,
                    cache_hit_input_per_million: None,
                    cache_miss_input_per_million: None,
                }),
                billing: None,
            },
            move |chunk| {
                chunks_for_callback
                    .lock()
                    .expect("chunks")
                    .push(chunk.to_string());
            },
        )
        .await
        .expect("stream chat");

    assert_eq!(
        streamed_chunks.lock().expect("chunks").join(""),
        "hello back"
    );
    assert_eq!(response.text, "hello back");
    let usage = response.metrics.usage.expect("stream usage");
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.output_tokens, 500);
    let cost = response.metrics.cost.expect("stream configured cost");
    assert!((cost.amount - 0.007).abs() < f64::EPSILON);
    assert_eq!(cost.source, CostSource::ConfiguredPricing);
    assert_eq!(debug.request.headers["authorization"], "Bearer [redacted]");
    assert_eq!(debug.request.body["stream"], true);
    assert_eq!(debug.response.status, 200);
    assert_eq!(debug.response.body["message"]["content"], "hello back");
    assert_eq!(debug.response.body["usage"]["input_tokens"], 1000);
    assert_eq!(debug.response.body["cost"]["source"], "configured-pricing");
}

#[tokio::test]
async fn chat_completion_falls_back_to_reasoning_content_when_content_is_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "content": "",
                    "reasoning_content": "reasoning fallback"
                },
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::DeepSeek,
        model: "deepseek-chat".to_string(),
        base_url: server.uri(),
        token_ref: "deepseek:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let response = client
        .chat_completion(
            &profile,
            "secret",
            ChatRequest {
                model: "deepseek-chat".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hello".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect("chat");

    assert_eq!(response.text, "reasoning fallback");
}

#[tokio::test]
async fn missing_token_behavior_is_clear() {
    let error = AppError::MissingToken {
        profile: "work".to_string(),
    };

    assert!(error.to_string().contains("ai token set --profile work"));
}

/// Баг 2: DeepSeek не имеет цены за input — расчёт должен считать только output
#[tokio::test]
async fn cost_calculated_with_output_only_pricing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::DeepSeek,
        model: "deepseek-chat".to_string(),
        base_url: server.uri(),
        token_ref: "deepseek:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let response = client
        .chat_completion(
            &profile,
            "secret",
            ChatRequest {
                model: "deepseek-chat".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: Some(ModelPricing {
                    currency: "USD".to_string(),
                    input_per_million: None, // DeepSeek: нет цены за input
                    output_per_million: 2.0,
                    cache_hit_input_per_million: None,
                    cache_miss_input_per_million: None,
                }),
                billing: None,
            },
        )
        .await
        .expect("chat");

    let cost = response
        .metrics
        .cost
        .expect("cost must be calculated even without input price");
    // 500 токенов * 2.0 / 1_000_000 = 0.001
    assert!(
        (cost.amount - 0.001).abs() < 1e-10,
        "actual: {}",
        cost.amount
    );
    assert_eq!(cost.currency, "USD");
}

/// Баг 4: ошибка провайдера должна содержать имя провайдера, не "unknown"
#[tokio::test]
async fn http_error_includes_provider_name_not_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenRouter,
        model: "some/model".to_string(),
        base_url: server.uri(),
        token_ref: "openrouter:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let error = client
        .chat_completion(
            &profile,
            "bad-token",
            ChatRequest {
                model: "some/model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect_err("should fail");

    let msg = error.to_string();
    assert!(
        msg.contains("openrouter"),
        "error должен содержать имя провайдера, получили: {msg}"
    );
    assert!(
        !msg.contains("'unknown'"),
        "не должно быть 'unknown', получили: {msg}"
    );
}

/// Баг 3: elapsed_ms должен отражать реальное время ответа
#[tokio::test]
async fn elapsed_ms_is_measured_after_full_body_received() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(50))
                .set_body_json(json!({
                    "choices": [{ "message": { "content": "slow reply" }, "finish_reason": "stop" }]
                })),
        )
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        base_url: server.uri(),
        token_ref: "openai-compatible:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let (response, _debug) = client
        .chat_completion_with_debug(
            &profile,
            "secret",
            ChatRequest {
                model: "test-model".to_string(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
                control: ResponseControl::uncontrolled(),
                pricing: None,
                billing: None,
            },
        )
        .await
        .expect("chat");

    assert_eq!(response.text, "slow reply");
    // 50мс задержки → elapsed должен быть хотя бы 30мс (CI может быть медленнее)
    assert!(
        response.metrics.elapsed_ms >= 30,
        "elapsed_ms={} слишком мало — таймер должен стартовать до отправки запроса",
        response.metrics.elapsed_ms
    );
}

/// Список моделей с ценами за output, но без input — должен возвращать pricing
#[tokio::test]
async fn list_models_with_output_only_pricing_returns_model_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(bearer_token("secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "provider/cheap-model",
                "pricing": { "completion": "0.000002" }
            }]
        })))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::OpenRouter,
        model: "provider/cheap-model".to_string(),
        base_url: server.uri(),
        token_ref: "openrouter:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let models = client
        .list_model_info(&profile, "secret")
        .await
        .expect("models");

    let model = models
        .iter()
        .find(|m| m.id == "provider/cheap-model")
        .expect("model");
    let pricing = model
        .pricing
        .as_ref()
        .expect("pricing должен присутствовать даже без input цены");
    assert!(
        pricing.input_per_million.is_none(),
        "input_per_million должен быть None"
    );
    assert!((pricing.output_per_million - 2.0).abs() < f64::EPSILON);
}

/// При отсутствии токена сообщение об ошибке содержит имя профиля и подсказку
#[tokio::test]
async fn missing_token_error_shows_profile_name_and_hint() {
    let error = AppError::MissingToken {
        profile: "my-profile".to_string(),
    };
    let msg = error.to_string();

    assert!(msg.contains("my-profile"), "должно содержать имя профиля");
    assert!(
        msg.contains("token set"),
        "должно содержать подсказку команды"
    );
}

#[tokio::test]
async fn rate_limit_maps_retry_after_from_provider() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "12"))
        .mount(&server)
        .await;

    let profile = ProfileConfig {
        provider: ProviderKind::Kimi,
        model: "moonshot-v1-8k".to_string(),
        base_url: server.uri(),
        token_ref: "kimi:test".to_string(),
    };
    let client = ReqwestProviderClient::new().expect("client");
    let error = client
        .list_models(&profile, "secret")
        .await
        .expect_err("rate limit");

    match error {
        AppError::ProviderHttp(http) => {
            assert_eq!(http.status, Some(StatusCode::TOO_MANY_REQUESTS));
            assert_eq!(
                http.problem,
                HttpProblem::RateLimit {
                    retry_after: Some("12".to_string())
                }
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
