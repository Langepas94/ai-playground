use std::{
    collections::HashMap,
    env,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use reqwest::{
    Client, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    errors::{AppError, EndpointCategory, HttpProblem, ProviderHttpError, map_http_status},
    providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader},
};

const EXTRA_HEADERS: &[StaticHeader] = &[];
const OAUTH_URL: &str = "https://ngw.devices.sberbank.ru:9443/api/v2/oauth";
const DEFAULT_SCOPE: &str = "GIGACHAT_API_PERS";
const TOKEN_EXPIRY_SKEW_MS: u64 = 60_000;
const FALLBACK_TOKEN_TTL_MS: u64 = 25 * 60 * 1000;

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::GigaChat,
        display_name: "GigaChat",
        default_base_url: "https://gigachat.devices.sberbank.ru/api/v1",
        default_model: "GigaChat",
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GigaChatTokenCache {
    tokens: Arc<Mutex<HashMap<String, CachedAccessToken>>>,
}

#[derive(Debug, Clone)]
struct CachedAccessToken {
    access_token: String,
    expires_at_ms: u64,
}

impl GigaChatTokenCache {
    fn get(&self, key: &str, now_ms: u64) -> Option<String> {
        self.tokens
            .lock()
            .ok()?
            .get(key)
            .filter(|token| token.expires_at_ms.saturating_sub(TOKEN_EXPIRY_SKEW_MS) > now_ms)
            .map(|token| token.access_token.clone())
    }

    fn insert(&self, key: String, access_token: String, expires_at_ms: u64) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(
                key,
                CachedAccessToken {
                    access_token,
                    expires_at_ms,
                },
            );
        }
    }

    pub(crate) fn invalidate(&self, key: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(key);
        }
    }
}

pub(crate) fn default_oauth_url() -> String {
    OAUTH_URL.to_string()
}

pub(crate) fn cache_key(stored_token: &str) -> String {
    format!("{}:{stored_token}", oauth_scope())
}

pub(crate) async fn bearer_token(
    client: &Client,
    stored_token: &str,
    cache: &GigaChatTokenCache,
    oauth_url: &str,
) -> Result<String, AppError> {
    if looks_like_access_token(stored_token) {
        return Ok(stored_token.to_string());
    }

    let key = cache_key(stored_token);
    if let Some(access_token) = cache.get(&key, now_ms()) {
        return Ok(access_token);
    }
    refresh_bearer_token(client, stored_token, cache, oauth_url).await
}

pub(crate) async fn refresh_bearer_token(
    client: &Client,
    stored_token: &str,
    cache: &GigaChatTokenCache,
    oauth_url: &str,
) -> Result<String, AppError> {
    if looks_like_access_token(stored_token) {
        return Ok(stored_token.to_string());
    }

    let scope = oauth_scope();
    let response = client
        .post(oauth_url)
        .header(AUTHORIZATION, format!("Basic {stored_token}"))
        .header("Accept", "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("RqUID", Uuid::new_v4().to_string())
        .form(&[("scope", scope)])
        .send()
        .await
        .map_err(map_oauth_network_error)?;
    let status = response.status();
    let body = response.text().await.map_err(AppError::from)?;

    if !status.is_success() {
        return Err(AppError::ProviderHttp(map_http_status(
            ProviderKind::GigaChat.to_string(),
            EndpointCategory::Other("oauth".to_string()),
            status,
            None,
            short_reason(&body),
        )));
    }

    let token: GigaChatOAuthResponse = serde_json::from_str(&body).map_err(|error| {
        AppError::ProviderHttp(ProviderHttpError {
            provider: ProviderKind::GigaChat.to_string(),
            endpoint: EndpointCategory::Other("oauth".to_string()),
            status: Some(StatusCode::OK),
            problem: HttpProblem::UnexpectedFormat,
            reason: error.to_string(),
        })
    })?;
    let access_token = token.access_token;
    let expires_at_ms = token
        .expires_at
        .map(normalize_expires_at_ms)
        .unwrap_or_else(|| now_ms().saturating_add(FALLBACK_TOKEN_TTL_MS));
    cache.insert(cache_key(stored_token), access_token.clone(), expires_at_ms);
    Ok(access_token)
}

pub(crate) fn looks_like_access_token(token: &str) -> bool {
    token.matches('.').count() >= 2
}

fn oauth_scope() -> String {
    env::var("AI_PLAYGROUND_GIGACHAT_SCOPE")
        .or_else(|_| env::var("AITEACH_GIGACHAT_SCOPE"))
        .unwrap_or_else(|_| DEFAULT_SCOPE.to_string())
}

fn normalize_expires_at_ms(value: u64) -> u64 {
    if value > 10_000_000_000 {
        value
    } else {
        value.saturating_mul(1000)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn map_oauth_network_error(error: reqwest::Error) -> AppError {
    AppError::ProviderHttp(ProviderHttpError {
        provider: ProviderKind::GigaChat.to_string(),
        endpoint: EndpointCategory::Other("oauth".to_string()),
        status: error.status(),
        problem: if error.is_decode() {
            HttpProblem::UnexpectedFormat
        } else {
            HttpProblem::Network
        },
        reason: error.to_string(),
    })
}

fn short_reason(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 180 {
        format!("{}...", collapsed.chars().take(180).collect::<String>())
    } else if collapsed.is_empty() {
        "empty response body".to_string()
    } else {
        collapsed
    }
}

#[derive(Debug, Deserialize)]
struct GigaChatOAuthResponse {
    access_token: String,
    expires_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gigachat_expires_at_accepts_seconds_and_milliseconds() {
        assert_eq!(normalize_expires_at_ms(1_679_471_442), 1_679_471_442_000);
        assert_eq!(
            normalize_expires_at_ms(1_706_026_848_841),
            1_706_026_848_841
        );
    }

    #[test]
    fn gigachat_cache_uses_expiry_with_safety_skew() {
        let cache = GigaChatTokenCache::default();
        cache.insert("key".to_string(), "access".to_string(), 120_000);

        assert_eq!(cache.get("key", 1_000).as_deref(), Some("access"));
        assert_eq!(cache.get("key", 70_000), None);
    }
}
