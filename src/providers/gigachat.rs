use std::env;

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

pub async fn bearer_token(client: &Client, stored_token: &str) -> Result<String, AppError> {
    if looks_like_access_token(stored_token) {
        return Ok(stored_token.to_string());
    }

    let scope = env::var("AI_PLAYGROUND_GIGACHAT_SCOPE")
        .or_else(|_| env::var("AITEACH_GIGACHAT_SCOPE"))
        .unwrap_or_else(|_| DEFAULT_SCOPE.to_string());
    let response = client
        .post(OAUTH_URL)
        .header(AUTHORIZATION, format!("Basic {stored_token}"))
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
    Ok(token.access_token)
}

fn looks_like_access_token(token: &str) -> bool {
    token.matches('.').count() >= 2
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
}
