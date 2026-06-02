use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[];

pub fn spec() -> ProviderSpec {
    ProviderSpec {
        kind: ProviderKind::DeepSeek,
        display_name: "DeepSeek",
        default_base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        auth_scheme: AuthScheme::Bearer,
        extra_headers: EXTRA_HEADERS,
    }
}
