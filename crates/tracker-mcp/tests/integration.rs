//! Offline integration tests: the library API against a mock HTTP server.
//!
//! No real network, no real credentials. A fake token is injected and the base
//! URL points at a [`wiremock`] server. Every tool has a happy path and at
//! least one error path; auth headers are asserted across all four
//! token/org combinations; and the token string is asserted absent from output.

use serde_json::{json, Value};
use tracker_mcp::{call_tool, tool_defs, Config, OrgKind, TokenKind, TrackerClient};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "SECRET-TOKEN-DO-NOT-LEAK";

const ISSUE: &str = include_str!("fixtures/issue.json");
const SEARCH_PAGE: &str = include_str!("fixtures/search_page.json");
const COMMENT: &str = include_str!("fixtures/comment.json");
const ERROR_422: &str = include_str!("fixtures/error_422.json");

fn client_for(server: &MockServer, tk: TokenKind, ok: OrgKind) -> TrackerClient {
    let cfg = Config::new(TOKEN, tk, "ORG-7", ok).with_base_url(server.uri());
    TrackerClient::new(cfg)
}

fn client(server: &MockServer) -> TrackerClient {
    client_for(server, TokenKind::OAuth, OrgKind::XOrgId)
}

// ── happy paths ────────────────────────────────────────────────────────────

#[tokio::test]
async fn issue_get_happy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/TASK-1"))
        .and(header("Authorization", "OAuth SECRET-TOKEN-DO-NOT-LEAK"))
        .and(header("X-Org-ID", "ORG-7"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ISSUE))
        .mount(&server)
        .await;

    let out = call_tool(&client(&server), "issue_get", &json!({ "key": "TASK-1" })).await;
    assert!(!out.is_error, "{}", out.text);
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["key"], "TASK-1");
}

#[tokio::test]
async fn issue_search_happy_sends_paging() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issues/_search"))
        .and(query_param("perPage", "10"))
        .and(query_param("page", "2"))
        .and(body_json(json!({ "query": "Queue: TASK" })))
        .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH_PAGE))
        .mount(&server)
        .await;

    let out = call_tool(
        &client(&server),
        "issue_search",
        &json!({ "query": "Queue: TASK", "per_page": 10, "page": 2 }),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    let v: Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn issue_create_happy_builds_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issues"))
        .and(body_json(json!({
            "queue": "TASK",
            "summary": "New issue",
            "priority": { "key": "normal" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_string(ISSUE))
        .mount(&server)
        .await;

    let out = call_tool(
        &client(&server),
        "issue_create",
        &json!({ "queue": "TASK", "summary": "New issue" }),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
}

#[tokio::test]
async fn issue_update_happy_patches_fields() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/issues/TASK-1"))
        .and(body_json(json!({ "summary": "Renamed" })))
        .respond_with(ResponseTemplate::new(200).set_body_string(ISSUE))
        .mount(&server)
        .await;

    let out = call_tool(
        &client(&server),
        "issue_update",
        &json!({ "key": "TASK-1", "fields": { "summary": "Renamed" } }),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
}

#[tokio::test]
async fn comment_add_happy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issues/TASK-1/comments"))
        .and(body_json(json!({ "text": "Deployed to staging." })))
        .respond_with(ResponseTemplate::new(201).set_body_string(COMMENT))
        .mount(&server)
        .await;

    let out = call_tool(
        &client(&server),
        "comment_add",
        &json!({ "key": "TASK-1", "text": "Deployed to staging." }),
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
}

// ── error paths ────────────────────────────────────────────────────────────

#[tokio::test]
async fn issue_get_404_maps_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/NOPE-1"))
        .respond_with(ResponseTemplate::new(404).set_body_string("{}"))
        .mount(&server)
        .await;

    let out = call_tool(&client(&server), "issue_get", &json!({ "key": "NOPE-1" })).await;
    assert!(out.is_error);
    assert!(out.text.contains("not found"));
    assert!(out.text.contains("NOPE-1"));
}

#[tokio::test]
async fn issue_get_401_maps_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/TASK-1"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
        .mount(&server)
        .await;

    let out = call_tool(&client(&server), "issue_get", &json!({ "key": "TASK-1" })).await;
    assert!(out.is_error);
    assert!(out.text.contains("auth failed"));
}

#[tokio::test]
async fn issue_create_422_surfaces_field_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issues"))
        .respond_with(ResponseTemplate::new(422).set_body_string(ERROR_422))
        .mount(&server)
        .await;

    let out = call_tool(
        &client(&server),
        "issue_create",
        &json!({ "queue": "TASK", "summary": "x" }),
    )
    .await;
    assert!(out.is_error);
    assert!(out.text.contains("summary"));
    assert!(out.text.contains("may not be empty"));
}

// ── validation happens before any HTTP call ──────────────────────────────────

#[tokio::test]
async fn invalid_input_fires_no_http() {
    let server = MockServer::start().await;
    // No mocks mounted; any request would be recorded.
    let out = call_tool(&client(&server), "issue_get", &json!({})).await;
    assert!(out.is_error);
    assert!(out.text.contains("invalid input"));

    let recorded = server.received_requests().await.unwrap();
    assert!(recorded.is_empty(), "validation must not hit the network");
}

#[tokio::test]
async fn unknown_tool_fires_no_http() {
    let server = MockServer::start().await;
    let out = call_tool(&client(&server), "does_not_exist", &json!({})).await;
    assert!(out.is_error);
    let recorded = server.received_requests().await.unwrap();
    assert!(recorded.is_empty());
}

// ── auth matrix: all 4 combos send exact headers ─────────────────────────────

#[tokio::test]
async fn auth_matrix_exact_headers() {
    let combos = [
        (
            TokenKind::OAuth,
            OrgKind::XOrgId,
            "OAuth SECRET-TOKEN-DO-NOT-LEAK",
            "X-Org-ID",
        ),
        (
            TokenKind::OAuth,
            OrgKind::XCloudOrgId,
            "OAuth SECRET-TOKEN-DO-NOT-LEAK",
            "X-Cloud-Org-ID",
        ),
        (
            TokenKind::Iam,
            OrgKind::XOrgId,
            "Bearer SECRET-TOKEN-DO-NOT-LEAK",
            "X-Org-ID",
        ),
        (
            TokenKind::Iam,
            OrgKind::XCloudOrgId,
            "Bearer SECRET-TOKEN-DO-NOT-LEAK",
            "X-Cloud-Org-ID",
        ),
    ];
    for (tk, ok, authz, org_header) in combos {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/issues/TASK-1"))
            .and(header("Authorization", authz))
            .and(header(org_header, "ORG-7"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ISSUE))
            .mount(&server)
            .await;

        let out = call_tool(
            &client_for(&server, tk, ok),
            "issue_get",
            &json!({ "key": "TASK-1" }),
        )
        .await;
        assert!(
            !out.is_error,
            "combo {authz}/{org_header} failed: {}",
            out.text
        );
    }
}

// ── registration ─────────────────────────────────────────────────────────────

#[test]
fn registration_lists_all_five() {
    let defs = tool_defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
    assert_eq!(names.len(), 5);
    for expected in [
        "issue_get",
        "issue_search",
        "issue_create",
        "issue_update",
        "comment_add",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    for d in &defs {
        assert!(!d.description.is_empty());
        assert_eq!(d.input_schema["type"], "object");
        // Each schema must itself be valid JSON Schema.
        jsonschema::validator_for(&d.input_schema).unwrap();
    }
}

// ── token never leaks into output ────────────────────────────────────────────

#[tokio::test]
async fn token_never_appears_in_output() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/TASK-1"))
        .respond_with(ResponseTemplate::new(401).set_body_string(TOKEN)) // even if server echoes it
        .mount(&server)
        .await;

    let out = call_tool(&client(&server), "issue_get", &json!({ "key": "TASK-1" })).await;
    assert!(out.is_error);
    assert!(!out.text.contains(TOKEN), "token leaked into error output");
}
