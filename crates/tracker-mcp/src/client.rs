//! The HTTP layer: one [`TrackerClient::request`] helper every tool funnels
//! through.
//!
//! Responsibilities, all in one place:
//! - inject auth headers via [`crate::auth::auth_headers`];
//! - build the URL from the (overridable) base URL;
//! - JSON encode the body / decode the response;
//! - apply a 10s per-attempt timeout;
//! - retry at most twice on `5xx` with linear backoff; never retry `4xx`.

use crate::auth::auth_headers;
use crate::config::Config;
use crate::error::TrackerError;
use serde_json::Value;
use std::time::Duration;

/// Per-attempt request timeout.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum number of retries (so up to `MAX_RETRIES + 1` attempts) on `5xx`.
const MAX_RETRIES: u32 = 2;

/// HTTP method for a Tracker request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PATCH`
    Patch,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
        }
    }
}

/// A configured Tracker HTTP client.
///
/// Holds the [`Config`] and a reusable [`reqwest::Client`]. Cheap to clone the
/// config in; construct once and share via `&self`.
#[derive(Debug, Clone)]
pub struct TrackerClient {
    config: Config,
    http: reqwest::Client,
}

impl TrackerClient {
    /// Build a client from a [`Config`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::{Config, TokenKind, OrgKind, TrackerClient};
    /// let cfg = Config::new("t", TokenKind::OAuth, "o", OrgKind::XOrgId);
    /// let _client = TrackerClient::new(cfg);
    /// ```
    pub fn new(config: Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        TrackerClient { config, http }
    }

    /// Borrow the active configuration (token stays masked in `Debug`).
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Perform a request and decode the JSON response.
    ///
    /// `path` is appended to the configured base URL (leading slash optional).
    /// `resource` is a human label (e.g. an issue key) used to build a `404`
    /// message. `query` are URL query pairs. `body` is an optional JSON body.
    ///
    /// Returns the parsed response body on `2xx`, or a mapped
    /// [`TrackerError`] otherwise. Retries `5xx` up to twice; `4xx` is final.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        resource: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, TrackerError> {
        let url = format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        let mut attempt = 0u32;
        loop {
            let mut req = self
                .http
                .request(
                    reqwest::Method::from_bytes(method.as_str().as_bytes())
                        .expect("static method is valid"),
                    &url,
                )
                .query(query)
                .header("Content-Type", "application/json");

            for (name, value) in auth_headers(&self.config) {
                req = req.header(name, value);
            }
            if let Some(b) = body {
                req = req.json(b);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    // Network/timeout: retry like a 5xx, then give up.
                    if attempt < MAX_RETRIES {
                        attempt += 1;
                        backoff(attempt).await;
                        continue;
                    }
                    return Err(TrackerError::Transport(sanitize(e.to_string())));
                }
            };

            let status = resp.status().as_u16();
            if status >= 500 && attempt < MAX_RETRIES {
                attempt += 1;
                backoff(attempt).await;
                continue;
            }

            let text = resp
                .text()
                .await
                .map_err(|e| TrackerError::Transport(sanitize(e.to_string())))?;

            if (200..300).contains(&status) {
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text)
                    .map_err(|e| TrackerError::Decode(e.to_string()));
            }
            return Err(TrackerError::from_http(status, resource, &text));
        }
    }
}

/// Linear backoff between retries (100ms × attempt).
async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
}

/// Strip anything token-ish from a transport error string, just in case a URL
/// with embedded credentials ever appears. Defensive; Tracker auth is header
/// based so this is normally a no-op.
fn sanitize(s: String) -> String {
    s.replace("OAuth ", "OAuth ***")
        .replace("Bearer ", "Bearer ***")
}
