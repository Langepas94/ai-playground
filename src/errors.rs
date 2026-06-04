use std::path::PathBuf;

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointCategory {
    Models,
    Chat,
    Other(String),
}

impl std::fmt::Display for EndpointCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Models => write!(f, "models"),
            Self::Chat => write!(f, "chat"),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpProblem {
    Auth,
    RateLimit { retry_after: Option<String> },
    Provider,
    UnexpectedFormat,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpError {
    pub provider: String,
    pub endpoint: EndpointCategory,
    pub status: Option<StatusCode>,
    pub problem: HttpProblem,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Config error at {path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("Profile '{0}' was not found")]
    ProfileMissing(String),
    #[error("No active profile. Run `ai-playground profile use <name>` first")]
    NoActiveProfile,
    #[error(
        "Token is missing for profile '{profile}'. Run `ai-playground token set --profile {profile}`"
    )]
    MissingToken { profile: String },
    #[error("Secret storage error: {0}")]
    Secret(String),
    #[error("Terminal I/O error: {0}")]
    Terminal(String),
    #[error("{0}")]
    ProviderHttp(ProviderHttpError),
    #[error("Provider response has an unexpected JSON format: {0}")]
    Json(String),
    #[error("Unsupported provider: {0}")]
    UnsupportedProvider(String),
    #[error("Invalid base_url for profile '{profile}': {url}")]
    InvalidBaseUrl { profile: String, url: String },
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self
            .status
            .map(|value| value.as_u16().to_string())
            .unwrap_or_else(|| "no HTTP status".to_string());
        match &self.problem {
            HttpProblem::Auth => write!(
                f,
                "Authentication failed for provider '{}' on {} endpoint (status {status}). Check that the token exists, is not expired, and belongs to this provider.",
                self.provider, self.endpoint
            ),
            HttpProblem::RateLimit { retry_after } => {
                if let Some(retry_after) = retry_after {
                    write!(
                        f,
                        "Rate limit from provider '{}' on {} endpoint (status {status}). Retry after {retry_after}.",
                        self.provider, self.endpoint
                    )
                } else {
                    write!(
                        f,
                        "Rate limit from provider '{}' on {} endpoint (status {status}). Try again later.",
                        self.provider, self.endpoint
                    )
                }
            }
            HttpProblem::UnexpectedFormat => write!(
                f,
                "Provider '{}' returned an unexpected response format on {} endpoint (status {status}): {}",
                self.provider, self.endpoint, self.reason
            ),
            HttpProblem::Network => write!(
                f,
                "Network error while contacting provider '{}' on {} endpoint. Check DNS, TLS, proxy settings, and timeout: {}",
                self.provider, self.endpoint, self.reason
            ),
            HttpProblem::Provider => write!(
                f,
                "Provider '{}' error on {} endpoint (status {status}): {}",
                self.provider, self.endpoint, self.reason
            ),
        }
    }
}

pub fn map_http_status(
    provider: impl Into<String>,
    endpoint: EndpointCategory,
    status: StatusCode,
    retry_after: Option<String>,
    reason: impl Into<String>,
) -> ProviderHttpError {
    let problem = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => HttpProblem::Auth,
        StatusCode::TOO_MANY_REQUESTS => HttpProblem::RateLimit { retry_after },
        _ => HttpProblem::Provider,
    };

    ProviderHttpError {
        provider: provider.into(),
        endpoint,
        status: Some(status),
        problem,
        reason: reason.into(),
    }
}

impl From<reqwest::Error> for AppError {
    fn from(error: reqwest::Error) -> Self {
        let problem = if error.is_decode() {
            HttpProblem::UnexpectedFormat
        } else {
            HttpProblem::Network
        };
        Self::ProviderHttp(ProviderHttpError {
            provider: "unknown".to_string(),
            endpoint: EndpointCategory::Other("unknown".to_string()),
            status: error.status(),
            problem,
            reason: error.to_string(),
        })
    }
}
