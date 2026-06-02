use reqwest::Client;

use crate::{
    config::ProfileConfig,
    errors::{AppError, EndpointCategory},
    providers::{
        ChatRequest, ChatResponse, chat_payload, map_network_error, parse_chat_response,
        parse_models_response, response_text_or_error,
    },
};

pub async fn list_models(
    client: &Client,
    profile: &ProfileConfig,
    token: &str,
) -> Result<Vec<String>, AppError> {
    let url = endpoint(&profile.base_url, "models");
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| map_network_error(&profile.provider, EndpointCategory::Models, error))?;
    let raw = response_text_or_error(response, &profile.provider, EndpointCategory::Models).await?;
    parse_models_response(&profile.provider, &raw)
}

pub async fn chat_completion(
    client: &Client,
    profile: &ProfileConfig,
    token: &str,
    request: ChatRequest,
) -> Result<ChatResponse, AppError> {
    let url = endpoint(&profile.base_url, "chat/completions");
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(&chat_payload(request))
        .send()
        .await
        .map_err(|error| map_network_error(&profile.provider, EndpointCategory::Chat, error))?;
    let raw = response_text_or_error(response, &profile.provider, EndpointCategory::Chat).await?;
    parse_chat_response(&profile.provider, &raw)
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path)
}
