use ai_playground::{
    config::ProfileConfig,
    errors::{AppError, HttpProblem},
    providers::{
        ChatMessage, ChatRequest, ProviderClient, ProviderKind, ReqwestProviderClient,
        ResponseControl, Role,
    },
};
use reqwest::StatusCode;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{bearer_token, method, path},
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
            },
        )
        .await
        .expect("chat");

    assert_eq!(response.text, "hello back");
}

#[tokio::test]
async fn missing_token_behavior_is_clear() {
    let error = AppError::MissingToken {
        profile: "work".to_string(),
    };

    assert!(
        error
            .to_string()
            .contains("ai-playground token set --profile work")
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
