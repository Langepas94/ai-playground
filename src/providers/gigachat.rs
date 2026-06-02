use crate::providers::{AuthScheme, ProviderKind, ProviderSpec, StaticHeader};

const EXTRA_HEADERS: &[StaticHeader] = &[];

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
