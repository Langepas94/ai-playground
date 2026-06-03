use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::Kimi,
        display_name: "Kimi",
        default_base_url: "https://api.moonshot.ai/v1",
        default_model: "moonshot-v1-8k",
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
