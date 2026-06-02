use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[];
const SUGGESTED_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::DeepSeek,
        display_name: "DeepSeek",
        default_base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        suggested_models: SUGGESTED_MODELS,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
