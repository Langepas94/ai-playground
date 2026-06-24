//! Connection + auth configuration.
//!
//! [`Config`] is the single source of truth for *how* to reach Tracker: which
//! token scheme, which organization header, and which base URL. It is built
//! either explicitly (tests, embedding) or from environment variables
//! ([`Config::from_env`]).
//!
//! The token is wrapped in [`Secret`] so it can never be printed by accident:
//! `Debug` renders it as `***`.

use crate::error::TrackerError;
use std::fmt;

/// Default production base URL for Tracker REST API v3.
pub const DEFAULT_BASE_URL: &str = "https://api.tracker.yandex.net/v3";

/// Token scheme used in the `Authorization` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Yandex OAuth token → `Authorization: OAuth <token>`.
    OAuth,
    /// Yandex Cloud IAM token → `Authorization: Bearer <token>`.
    Iam,
}

impl TokenKind {
    /// Parse from the `TRACKER_TOKEN_KIND` value. Defaults handled by caller.
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::TokenKind;
    /// assert_eq!(TokenKind::parse("iam").unwrap(), TokenKind::Iam);
    /// assert!(TokenKind::parse("nope").is_err());
    /// ```
    pub fn parse(s: &str) -> Result<Self, TrackerError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "oauth" => Ok(TokenKind::OAuth),
            "iam" | "bearer" => Ok(TokenKind::Iam),
            other => Err(TrackerError::Config(format!(
                "unknown TRACKER_TOKEN_KIND '{other}' (expected oauth | iam)"
            ))),
        }
    }
}

/// Which organization header Tracker expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgKind {
    /// `X-Org-ID` — Yandex 360 / standalone Tracker.
    XOrgId,
    /// `X-Cloud-Org-ID` — Yandex Cloud Organization.
    XCloudOrgId,
}

impl OrgKind {
    /// Parse from the `TRACKER_ORG_KIND` value.
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::OrgKind;
    /// assert_eq!(OrgKind::parse("x_cloud_org_id").unwrap(), OrgKind::XCloudOrgId);
    /// ```
    pub fn parse(s: &str) -> Result<Self, TrackerError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "x_org_id" | "x-org-id" | "org" => Ok(OrgKind::XOrgId),
            "x_cloud_org_id" | "x-cloud-org-id" | "cloud" => Ok(OrgKind::XCloudOrgId),
            other => Err(TrackerError::Config(format!(
                "unknown TRACKER_ORG_KIND '{other}' (expected x_org_id | x_cloud_org_id)"
            ))),
        }
    }

    /// The HTTP header name this org kind serializes to.
    pub fn header_name(self) -> &'static str {
        match self {
            OrgKind::XOrgId => "X-Org-ID",
            OrgKind::XCloudOrgId => "X-Cloud-Org-ID",
        }
    }
}

/// A secret string that refuses to reveal itself via `Debug`/`Display`.
///
/// Use [`Secret::expose`] only where the raw value is genuinely needed
/// (building the `Authorization` header). Never log the exposed value.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wrap a raw secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Borrow the raw value. Caller must not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// A masked, log-safe rendering (`***` or `abc***` for non-empty values).
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::Secret;
    /// assert_eq!(Secret::new("supersecrettoken").masked(), "sup***");
    /// assert_eq!(Secret::new("").masked(), "***");
    /// ```
    pub fn masked(&self) -> String {
        if self.0.is_empty() {
            "***".to_string()
        } else {
            let prefix: String = self.0.chars().take(3).collect();
            format!("{prefix}***")
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Everything needed to authenticate and address Tracker.
///
/// Construct directly for embedding/tests, or via [`Config::from_env`] for the
/// standalone binary. `Debug` is safe: the token is masked.
#[derive(Debug, Clone)]
pub struct Config {
    /// API token (OAuth or IAM). Masked in all output.
    pub token: Secret,
    /// Token scheme. Default [`TokenKind::OAuth`].
    pub token_kind: TokenKind,
    /// Organization id value sent in the org header.
    pub org_id: String,
    /// Which org header to use. Default [`OrgKind::XOrgId`].
    pub org_kind: OrgKind,
    /// Base URL; override in tests to point at a mock server.
    pub base_url: String,
}

impl Config {
    /// Build a config explicitly (embedding / tests).
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::{Config, TokenKind, OrgKind};
    /// let cfg = Config::new("tkn", TokenKind::OAuth, "org-1", OrgKind::XOrgId)
    ///     .with_base_url("http://localhost:9999");
    /// assert_eq!(cfg.base_url, "http://localhost:9999");
    /// ```
    pub fn new(
        token: impl Into<String>,
        token_kind: TokenKind,
        org_id: impl Into<String>,
        org_kind: OrgKind,
    ) -> Self {
        Config {
            token: Secret::new(token),
            token_kind,
            org_id: org_id.into(),
            org_kind,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Override the base URL (used by tests to target a mock server).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Build from environment variables.
    ///
    /// Required: `TRACKER_TOKEN`, `TRACKER_ORG_ID`.
    /// Optional: `TRACKER_TOKEN_KIND` (default `oauth`),
    /// `TRACKER_ORG_KIND` (default `x_org_id`),
    /// `TRACKER_BASE_URL` (default production).
    ///
    /// Fails fast with a clear, token-free message if a required value is
    /// missing.
    pub fn from_env() -> Result<Self, TrackerError> {
        let token = require_env("TRACKER_TOKEN")?;
        let org_id = require_env("TRACKER_ORG_ID")?;
        let token_kind = match std::env::var("TRACKER_TOKEN_KIND") {
            Ok(v) if !v.trim().is_empty() => TokenKind::parse(&v)?,
            _ => TokenKind::OAuth,
        };
        let org_kind = match std::env::var("TRACKER_ORG_KIND") {
            Ok(v) if !v.trim().is_empty() => OrgKind::parse(&v)?,
            _ => OrgKind::XOrgId,
        };
        let base_url = match std::env::var("TRACKER_BASE_URL") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => DEFAULT_BASE_URL.to_string(),
        };
        Ok(Config {
            token: Secret::new(token),
            token_kind,
            org_id,
            org_kind,
            base_url,
        })
    }
}

fn require_env(key: &str) -> Result<String, TrackerError> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(TrackerError::Config(format!(
            "missing required environment variable {key}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_leaks() {
        let s = Secret::new("supersecret-token-value");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{s}"), "***");
        assert!(!s.masked().contains("secret-token"));
    }

    #[test]
    fn config_debug_masks_token() {
        let cfg = Config::new(
            "supersecret-token-value",
            TokenKind::OAuth,
            "o",
            OrgKind::XOrgId,
        );
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("supersecret-token-value"));
    }

    #[test]
    fn org_header_names() {
        assert_eq!(OrgKind::XOrgId.header_name(), "X-Org-ID");
        assert_eq!(OrgKind::XCloudOrgId.header_name(), "X-Cloud-Org-ID");
    }
}
