use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::errors::AppError;

#[derive(Debug)]
pub(crate) struct WebError(pub(crate) AppError);

pub(crate) struct ApiJson<T>(pub(crate) T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = WebError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error| {
                WebError(AppError::InvalidInput(match error {
                    JsonRejection::JsonDataError(error) => {
                        format!("Invalid JSON body: {error}")
                    }
                    JsonRejection::JsonSyntaxError(error) => {
                        format!("Malformed JSON body: {error}")
                    }
                    JsonRejection::MissingJsonContentType(_) => {
                        "Expected request body with content-type application/json".to_string()
                    }
                    other => format!("Invalid JSON request: {other}"),
                }))
            })
    }
}

impl From<AppError> for WebError {
    fn from(error: AppError) -> Self {
        Self(error)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = match self.0 {
            AppError::InvalidInput(_) | AppError::InvalidBaseUrl { .. } => StatusCode::BAD_REQUEST,
            AppError::ProviderHttp(_) | AppError::Mcp(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Json(ErrorResponse {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}
