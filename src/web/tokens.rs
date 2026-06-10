use crate::{
    config::{AppConfig, ProfileConfig, token_ref},
    errors::AppError,
    secrets::{SecretStore, get_config_profile_token, set_profile_token},
};

use super::util::parse_provider;

pub(crate) fn resolve_web_token(
    secrets: &dyn SecretStore,
    profile: &ProfileConfig,
    token_override: &str,
    token_provider: Option<&str>,
) -> Result<String, AppError> {
    let token_override = token_override.trim();
    if !token_override.is_empty() && token_override_belongs_to_provider(profile, token_provider)? {
        set_profile_token(secrets, profile, token_override)?;
        return Ok(token_override.to_string());
    }

    if let Some(token) = secrets.get_token(&token_ref(&profile.provider))? {
        return Ok(token);
    }

    let config = AppConfig::load()?;
    for (name, candidate) in &config.profiles {
        if candidate.provider != profile.provider {
            continue;
        }
        if let Some(token) = get_config_profile_token(secrets, &config, name, candidate)? {
            return Ok(token);
        }
    }

    Err(AppError::InvalidInput(
        "API token is required. Save it with `ai token set --profile <name>` or paste it once in the web UI.".to_string(),
    ))
}

pub(crate) fn web_token_present(
    secrets: &dyn SecretStore,
    profile: &ProfileConfig,
    token_override: &str,
    token_provider: Option<&str>,
) -> Result<bool, AppError> {
    if !token_override.trim().is_empty()
        && token_override_belongs_to_provider(profile, token_provider)?
    {
        return Ok(true);
    }
    if secrets.get_token(&token_ref(&profile.provider))?.is_some() {
        return Ok(true);
    }
    let config = AppConfig::load()?;
    for (name, candidate) in &config.profiles {
        if candidate.provider != profile.provider {
            continue;
        }
        if get_config_profile_token(secrets, &config, name, candidate)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn token_override_belongs_to_provider(
    profile: &ProfileConfig,
    token_provider: Option<&str>,
) -> Result<bool, AppError> {
    let Some(token_provider) = token_provider else {
        return Ok(true);
    };
    let token_provider = token_provider.trim();
    if token_provider.is_empty() {
        return Ok(false);
    }
    Ok(parse_provider(token_provider)? == profile.provider)
}
