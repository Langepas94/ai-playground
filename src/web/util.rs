use crate::{errors::AppError, providers::ProviderKind};

pub(crate) fn parse_provider(value: &str) -> Result<ProviderKind, AppError> {
    value.parse()
}

pub(crate) fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(crate) fn blank_str_to_none(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
