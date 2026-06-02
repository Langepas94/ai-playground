use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[];
const SUGGESTED_MODELS: &[&str] = &["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::Kimi,
        display_name: "Kimi",
        default_base_url: "https://api.moonshot.ai/v1",
        default_model: "moonshot-v1-8k",
        suggested_models: SUGGESTED_MODELS,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
