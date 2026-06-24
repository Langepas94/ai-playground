//! The one place auth headers are built.
//!
//! [`auth_headers`] turns a [`Config`] into the exact header list Tracker
//! expects. Both the token scheme (`OAuth` vs `Bearer`) and the org header
//! variant come from config — nothing is hardcoded. Unit-tested across all
//! four `{token_kind} × {org_kind}` combinations.

use crate::config::{Config, TokenKind};

/// Build the `(name, value)` header pairs for a request.
///
/// Returns exactly two headers:
/// 1. `Authorization` — `OAuth <token>` or `Bearer <token>` per
///    [`TokenKind`].
/// 2. The org header — `X-Org-ID` or `X-Cloud-Org-ID` per
///    [`OrgKind`](crate::OrgKind).
///
/// The token is exposed here (it must be, to sign the request) but is never
/// logged by this function.
///
/// # Examples
///
/// ```
/// use tracker_mcp::{Config, TokenKind, OrgKind};
/// use tracker_mcp::auth::auth_headers;
///
/// let cfg = Config::new("t0ken", TokenKind::OAuth, "org-42", OrgKind::XOrgId);
/// let headers = auth_headers(&cfg);
/// assert_eq!(headers[0], ("Authorization".into(), "OAuth t0ken".into()));
/// assert_eq!(headers[1], ("X-Org-ID".into(), "org-42".into()));
/// ```
pub fn auth_headers(config: &Config) -> Vec<(String, String)> {
    let authorization = match config.token_kind {
        TokenKind::OAuth => format!("OAuth {}", config.token.expose()),
        TokenKind::Iam => format!("Bearer {}", config.token.expose()),
    };
    vec![
        ("Authorization".to_string(), authorization),
        (
            config.org_kind.header_name().to_string(),
            config.org_id.clone(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrgKind;

    fn headers_for(tk: TokenKind, ok: OrgKind) -> Vec<(String, String)> {
        auth_headers(&Config::new("TKN", tk, "ORG", ok))
    }

    #[test]
    fn oauth_x_org_id() {
        let h = headers_for(TokenKind::OAuth, OrgKind::XOrgId);
        assert_eq!(h[0], ("Authorization".into(), "OAuth TKN".into()));
        assert_eq!(h[1], ("X-Org-ID".into(), "ORG".into()));
    }

    #[test]
    fn oauth_x_cloud_org_id() {
        let h = headers_for(TokenKind::OAuth, OrgKind::XCloudOrgId);
        assert_eq!(h[0], ("Authorization".into(), "OAuth TKN".into()));
        assert_eq!(h[1], ("X-Cloud-Org-ID".into(), "ORG".into()));
    }

    #[test]
    fn iam_x_org_id() {
        let h = headers_for(TokenKind::Iam, OrgKind::XOrgId);
        assert_eq!(h[0], ("Authorization".into(), "Bearer TKN".into()));
        assert_eq!(h[1], ("X-Org-ID".into(), "ORG".into()));
    }

    #[test]
    fn iam_x_cloud_org_id() {
        let h = headers_for(TokenKind::Iam, OrgKind::XCloudOrgId);
        assert_eq!(h[0], ("Authorization".into(), "Bearer TKN".into()));
        assert_eq!(h[1], ("X-Cloud-Org-ID".into(), "ORG".into()));
    }
}
