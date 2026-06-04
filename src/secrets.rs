use std::{collections::HashMap, sync::Mutex};

use keyring::Entry;

use crate::{
    config::{AppConfig, ProfileConfig, legacy_token_ref, token_ref},
    errors::AppError,
};

const SERVICE: &str = "ai-playground";
const LEGACY_SERVICE: &str = "aiteach";

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
            Err(keyring::Error::NoEntry) => match Entry::new(LEGACY_SERVICE, token_ref)
                .map_err(|error| AppError::Secret(error.to_string()))?
                .get_password()
            {
                Ok(token) => {
                    self.set_token(token_ref, &token)?;
                    Ok(Some(token))
                }
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(AppError::Secret(error.to_string())),
            },
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
        }?;
        match Entry::new(LEGACY_SERVICE, token_ref)
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

pub fn get_config_profile_token(
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<Option<String>, AppError> {
    if let Some(token) = get_profile_token(secrets, profile_name, profile)? {
        return Ok(Some(token));
    }

    for (candidate_name, candidate_profile) in &config.profiles {
        if candidate_name == profile_name || candidate_profile.provider != profile.provider {
            continue;
        }
        if let Some(token) = get_profile_token(secrets, candidate_name, candidate_profile)? {
            set_profile_token(secrets, profile, &token)?;
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

    #[test]
    fn provider_tokens_fallback_to_legacy_token_from_another_profile() {
        let secrets = MemorySecretStore::default();
        let mut config = AppConfig::default();
        config.profiles.insert(
            "Deepseek".to_string(),
            ProfileConfig {
                provider: crate::providers::ProviderKind::DeepSeek,
                model: "deepseek-chat".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                token_ref: "deepseek:Deepseek".to_string(),
            },
        );
        let pro_profile = ProfileConfig {
            provider: crate::providers::ProviderKind::DeepSeek,
            model: "deepseek-v4-pro".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            token_ref: "deepseek:ВуDeepSeek pro".to_string(),
        };
        config
            .profiles
            .insert("ВуDeepSeek pro".to_string(), pro_profile.clone());

        secrets
            .set_token("deepseek:Deepseek", "legacy-token")
            .expect("set legacy token");
        assert_eq!(
            get_config_profile_token(&secrets, &config, "ВуDeepSeek pro", &pro_profile)
                .expect("get shared provider token"),
            Some("legacy-token".to_string())
        );
        assert_eq!(
            secrets.get_token("deepseek").expect("get migrated token"),
            Some("legacy-token".to_string())
        );
    }
}
