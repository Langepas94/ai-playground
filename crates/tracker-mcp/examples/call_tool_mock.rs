//! Call a tool against a local mock Tracker — no real credentials, no network.
//!
//! Demonstrates the embed path: build a [`Config`] pointed at a mock base URL,
//! make a [`TrackerClient`], and invoke [`call_tool`].
//!
//! Run with: `cargo run -p tracker-mcp --example call_tool_mock`

use serde_json::json;
use tracker_mcp::{call_tool, Config, OrgKind, TokenKind, TrackerClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::main]
async fn main() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/TASK-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"key":"TASK-1","summary":"Fix login redirect loop"}"#),
        )
        .mount(&server)
        .await;

    let cfg = Config::new("fake-token", TokenKind::OAuth, "org-1", OrgKind::XOrgId)
        .with_base_url(server.uri());
    let client = TrackerClient::new(cfg);

    let out = call_tool(&client, "issue_get", &json!({ "key": "TASK-1" })).await;
    println!("is_error = {}", out.is_error);
    println!("{}", out.text);
}
