//! Error type for the Tracker library and the status → message mapping.
//!
//! Every fallible public operation returns [`TrackerError`]. The variants map
//! one-to-one onto the user-facing messages required by the MCP result
//! contract (see [`crate::call_tool`]). A token value is **never** included in
//! any variant — error text is built from status codes and Tracker response
//! bodies only.

use thiserror::Error;

/// All failures surfaced by the library.
///
/// Display strings are safe to return to an MCP client: they never contain the
/// auth token. HTTP failures are pre-mapped to friendly messages by
/// [`TrackerError::from_http`].
#[derive(Debug, Error)]
pub enum TrackerError {
    /// Missing or malformed configuration (e.g. empty token / org id).
    #[error("config error: {0}")]
    Config(String),

    /// Input failed JSON Schema validation before any HTTP call was made.
    ///
    /// The message lists the offending fields.
    #[error("invalid input: {0}")]
    Validation(String),

    /// `401` — credentials rejected by Tracker.
    #[error("auth failed: check token + token_kind + org header")]
    Auth,

    /// `403` — authenticated but not allowed to touch the resource.
    #[error("forbidden: no access to queue/issue")]
    Forbidden,

    /// `404` — the named resource does not exist.
    #[error("not found: {0} does not exist")]
    NotFound(String),

    /// `422` — Tracker rejected the body; carries Tracker's field errors.
    #[error("validation: {0}")]
    Unprocessable(String),

    /// Any other non-2xx status.
    #[error("tracker error (status {status}): {message}")]
    Http {
        /// HTTP status code returned by Tracker.
        status: u16,
        /// Short, token-free description of the failure.
        message: String,
    },

    /// Transport-level failure (timeout, DNS, TLS, connection reset, …).
    #[error("transport error: {0}")]
    Transport(String),

    /// Unexpected/invalid JSON in a `2xx` response body.
    #[error("decode error: {0}")]
    Decode(String),
}

impl TrackerError {
    /// Map an HTTP status + response body to a friendly, token-free error.
    ///
    /// `resource` names what was being addressed (e.g. an issue key) and is
    /// interpolated into the `404` message. `body` is Tracker's raw response
    /// body, used to surface field errors on `422`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::TrackerError;
    ///
    /// let err = TrackerError::from_http(404, "TASK-1", "");
    /// assert_eq!(err.to_string(), "not found: TASK-1 does not exist");
    ///
    /// let err = TrackerError::from_http(401, "TASK-1", "");
    /// assert!(matches!(err, TrackerError::Auth));
    /// ```
    pub fn from_http(status: u16, resource: &str, body: &str) -> Self {
        match status {
            401 => TrackerError::Auth,
            403 => TrackerError::Forbidden,
            404 => TrackerError::NotFound(resource.to_string()),
            422 => TrackerError::Unprocessable(summarize_body(body)),
            other => TrackerError::Http {
                status: other,
                message: summarize_body(body),
            },
        }
    }
}

/// Reduce a Tracker error body to a compact, single-line message.
///
/// Tracker v3 error bodies look like
/// `{"errors":{"summary":["..."]},"errorMessages":["..."]}`. We prefer the
/// structured `errors` map, fall back to `errorMessages`, then to the raw body.
fn summarize_body(body: &str) -> String {
    if body.trim().is_empty() {
        return "no details".to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(errors) = v.get("errors").and_then(|e| e.as_object()) {
            let parts: Vec<String> = errors
                .iter()
                .map(|(field, msgs)| {
                    let joined = msgs
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|m| m.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_else(|| msgs.to_string());
                    format!("{field}: {joined}")
                })
                .collect();
            if !parts.is_empty() {
                return parts.join("; ");
            }
        }
        if let Some(msgs) = v.get("errorMessages").and_then(|m| m.as_array()) {
            let joined = msgs
                .iter()
                .filter_map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if !joined.is_empty() {
                return joined;
            }
        }
    }
    body.trim().chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_each_status() {
        assert!(matches!(
            TrackerError::from_http(401, "X", ""),
            TrackerError::Auth
        ));
        assert!(matches!(
            TrackerError::from_http(403, "X", ""),
            TrackerError::Forbidden
        ));
        assert!(matches!(
            TrackerError::from_http(404, "X", ""),
            TrackerError::NotFound(_)
        ));
        assert!(matches!(
            TrackerError::from_http(422, "X", ""),
            TrackerError::Unprocessable(_)
        ));
        assert!(matches!(
            TrackerError::from_http(500, "X", ""),
            TrackerError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn summarizes_tracker_field_errors() {
        let body = r#"{"errors":{"summary":["may not be empty"]}}"#;
        let err = TrackerError::from_http(422, "X", body);
        assert_eq!(err.to_string(), "validation: summary: may not be empty");
    }

    #[test]
    fn summarizes_error_messages() {
        let body = r#"{"errorMessages":["Queue not found"]}"#;
        let err = TrackerError::from_http(422, "X", body);
        assert_eq!(err.to_string(), "validation: Queue not found");
    }
}
