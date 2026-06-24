//! Thin **Streamable-HTTP** wrapper around the `tracker-mcp` library, for
//! remote deployment (containers / PaaS).
//!
//! Transport lives here and *only* here: it mounts the shared
//! [`TrackerServer`] handler on the `rmcp`
//! Streamable-HTTP tower service at `POST /mcp`, behind a Bearer auth gate, and
//! serves over plain HTTP (terminate TLS at the platform's edge / a reverse
//! proxy).
//!
//! # Security
//!
//! The tools include `issue_create` / `issue_update`, so an open endpoint = a
//! public write path into your Tracker under the server's token. This binary
//! **refuses to start without an auth gate**: set `MCP_AUTH_TOKEN` to a strong
//! secret (clients send `Authorization: Bearer <token>`), or explicitly opt out
//! with `MCP_ALLOW_NO_AUTH=1` for a trusted private network only.
//!
//! # Environment
//!
//! Tracker auth (same as the stdio binary): `TRACKER_TOKEN`, `TRACKER_ORG_ID`,
//! and optional `TRACKER_TOKEN_KIND` / `TRACKER_ORG_KIND` / `TRACKER_BASE_URL`.
//!
//! Server: `PORT` (default `8080`), `MCP_AUTH_TOKEN`, `MCP_ALLOW_NO_AUTH`,
//! `MCP_ALLOWED_HOSTS` (comma-separated allowlist for the `Host` header; unset
//! disables the check — fine behind the Bearer gate, required only to harden
//! against DNS-rebinding from browsers).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tracker_mcp::mcp::TrackerServer;
use tracker_mcp::{Config, TrackerClient};

/// Expected Bearer token. Empty string means auth is intentionally disabled.
type ExpectedToken = Arc<String>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracker = Config::from_env()?;
    let client = TrackerClient::new(tracker);

    let expected: ExpectedToken = Arc::new(resolve_auth()?);
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    // Build the Streamable-HTTP service over a fresh handler per session.
    let mut config = StreamableHttpServerConfig::default();

    // Stateless mode for serverless platforms: each request is independent,
    // container shuts down between calls → saves billing on long-running connections.
    let stateless = is_truthy("MCP_STATELESS");
    if stateless {
        eprintln!("tracker-mcp-http: MCP_STATELESS=1 — stateless JSON mode (serverless-optimized)");
        config = config.with_stateful_mode(false).with_json_response(true);
    }

    match allowed_hosts() {
        Some(hosts) => config = config.with_allowed_hosts(hosts),
        None => {
            eprintln!(
                "tracker-mcp-http: MCP_ALLOWED_HOSTS unset — Host check disabled \
                 (relying on the Bearer gate)."
            );
            config = config.disable_allowed_hosts();
        }
    }
    let service = StreamableHttpService::new(
        move || Ok(TrackerServer::new(client.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // /mcp is auth-gated; /health is open for platform probes.
    let mcp = Router::new()
        .nest_service("/mcp", service)
        .layer(from_fn_with_state(expected.clone(), auth_gate));
    let app = Router::new()
        .merge(mcp)
        .route("/health", get(|| async { "ok" }));

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("tracker-mcp-http: listening on http://{addr}/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Decide the Bearer token, failing closed: refuse to start with no gate unless
/// `MCP_ALLOW_NO_AUTH` is explicitly truthy.
fn resolve_auth() -> anyhow::Result<String> {
    match std::env::var("MCP_AUTH_TOKEN") {
        Ok(token) if !token.trim().is_empty() => Ok(token),
        _ if is_truthy("MCP_ALLOW_NO_AUTH") => {
            eprintln!("tracker-mcp-http: MCP_ALLOW_NO_AUTH set — running WITHOUT auth.");
            Ok(String::new())
        }
        _ => anyhow::bail!(
            "refusing to start without an auth gate: set MCP_AUTH_TOKEN=<secret> \
             (clients send `Authorization: Bearer <secret>`), or MCP_ALLOW_NO_AUTH=1 \
             for a trusted private network only"
        ),
    }
}

/// Parse `MCP_ALLOWED_HOSTS` into a non-empty allowlist, or `None`.
fn allowed_hosts() -> Option<Vec<String>> {
    let raw = std::env::var("MCP_ALLOWED_HOSTS").ok()?;
    let hosts: Vec<String> = raw
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    (!hosts.is_empty()).then_some(hosts)
}

fn is_truthy(var: &str) -> bool {
    matches!(
        std::env::var(var).ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Bearer auth middleware. When the expected token is empty, auth is disabled.
async fn auth_gate(State(expected): State<ExpectedToken>, req: Request, next: Next) -> Response {
    if expected.is_empty() {
        return next.run(req).await;
    }
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
            next.run(req).await
        }
        _ => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

/// Length-independent byte comparison to avoid leaking the token via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
