use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[
    StaticHeader {
        name: "HTTP-Referer",
        value: "https://ai-playground.local",
    },
    StaticHeader {
        name: "X-Title",
        value: "ai-playground",
    },
];
pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::OpenRouter,
        display_name: "OpenRouter",
        default_base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4.1-mini",
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
