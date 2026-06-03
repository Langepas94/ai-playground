use std::{collections::HashMap, sync::Mutex};

use keyring::Entry;

use crate::{
    config::{ProfileConfig, legacy_token_ref, token_ref},
    errors::AppError,
};

const SERVICE: &str = "aiteach";

pub trait SecretStore: Send + Sync {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError>;
    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError>;
    fn delete_token(&self, token_ref: &str) -> Result<(), AppError>;
}

#[derive(Debug, Default)]
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError> {
        Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .set_password(token)
            .map_err(|error| AppError::Secret(error.to_string()))
    }

    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        match Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get_password()
        {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }

    fn delete_token(&self, token_ref: &str) -> Result<(), AppError> {
        match Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .delete_credential()
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }
}

#[derive(Debug, Default)]
pub struct MemorySecretStore {
    tokens: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError> {
        self.tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .insert(token_ref.to_string(), token.to_string());
        Ok(())
    }

    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        Ok(self
            .tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get(token_ref)
            .cloned())
    }

    fn delete_token(&self, token_ref: &str) -> Result<(), AppError> {
        self.tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .remove(token_ref);
        Ok(())
    }
}

pub fn mask_token(token: &str) -> String {
    match token.len() {
        0 => "<empty>".to_string(),
        1..=8 => "****".to_string(),
        len => format!("{}...{}", &token[..4], &token[len - 4..]),
    }
}

pub fn profile_token_refs(profile_name: &str, profile: &ProfileConfig) -> Vec<String> {
    let current = token_ref(&profile.provider);
    let legacy = legacy_token_ref(&profile.provider, profile_name);
    if profile.token_ref == current || profile.token_ref == legacy {
        vec![current, legacy]
    } else {
        vec![current, profile.token_ref.clone(), legacy]
    }
}

pub fn get_profile_token(
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<Option<String>, AppError> {
    for token_ref in profile_token_refs(profile_name, profile) {
        if let Some(token) = secrets.get_token(&token_ref)? {
            return Ok(Some(token));
        }
    }
    Ok(None)
}

pub fn set_profile_token(
    secrets: &dyn SecretStore,
    profile: &ProfileConfig,
    token: &str,
) -> Result<(), AppError> {
    secrets.set_token(&token_ref(&profile.provider), token)
}

pub fn delete_profile_token(
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<(), AppError> {
    for token_ref in profile_token_refs(profile_name, profile) {
        secrets.delete_token(&token_ref)?;
    }
    Ok(())
}

pub fn delete_legacy_profile_token(
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<(), AppError> {
    secrets.delete_token(&legacy_token_ref(&profile.provider, profile_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_masking_never_prints_full_secret() {
        assert_eq!(mask_token(""), "<empty>");
        assert_eq!(mask_token("abc"), "****");
        assert_eq!(mask_token("sk-1234567890"), "sk-1...7890");
    }

    #[test]
    fn profile_tokens_prefer_provider_scope_and_fallback_to_legacy_scope() {
        let secrets = MemorySecretStore::default();
        let profile = ProfileConfig {
            provider: crate::providers::ProviderKind::DeepSeek,
            model: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token_ref: "deepseek:work".to_string(),
        };

        secrets
            .set_token("deepseek:work", "legacy-token")
            .expect("set legacy token");
        assert_eq!(
            get_profile_token(&secrets, "work", &profile).expect("get legacy"),
            Some("legacy-token".to_string())
        );

        set_profile_token(&secrets, &profile, "provider-token").expect("set provider token");
        assert_eq!(
            get_profile_token(&secrets, "work", &profile).expect("get provider"),
            Some("provider-token".to_string())
        );
    }
}
