use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[
    StaticHeader {
        name: "HTTP-Referer",
        value: "https://aiteach.local",
    },
    StaticHeader {
        name: "X-Title",
        value: "aiteach",
    },
];
const SUGGESTED_MODELS: &[&str] = &[
    "openai/gpt-4.1-mini",
    "openai/gpt-4.1",
    "deepseek/deepseek-chat",
    "google/gemini-2.0-flash-001",
];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::OpenRouter,
        display_name: "OpenRouter",
        default_base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4.1-mini",
        suggested_models: SUGGESTED_MODELS,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
