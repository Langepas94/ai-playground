use std::{collections::HashMap, fs, path::PathBuf, sync::Mutex};

use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSecrets {
    tokens: HashMap<String, String>,
}

impl SecretStore for KeyringSecretStore {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError> {
        let keyring_result = Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))
            .and_then(|entry| {
                entry
                    .set_password(token)
                    .map_err(|error| AppError::Secret(error.to_string()))
            });
        set_fallback_token(token_ref, token)?;
        keyring_result.or(Ok(()))
    }

    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        let keyring_token = self.get_keyring_token(token_ref).ok().flatten();
        if let Some(token) = keyring_token {
            return Ok(Some(token));
        }
        get_fallback_token(token_ref)
    }

    fn delete_token(&self, token_ref: &str) -> Result<(), AppError> {
        let _ = delete_keyring_token(SERVICE, token_ref);
        let _ = delete_keyring_token(LEGACY_SERVICE, token_ref);
        delete_fallback_token(token_ref)
    }
}

impl KeyringSecretStore {
    fn get_keyring_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        match Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get_password()
        {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => self.get_legacy_keyring_token(token_ref),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }

    fn get_legacy_keyring_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        match Entry::new(LEGACY_SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get_password()
        {
            Ok(token) => {
                self.set_token(token_ref, &token)?;
                Ok(Some(token))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }
}

fn delete_keyring_token(service: &str, token_ref: &str) -> Result<(), AppError> {
    match Entry::new(service, token_ref)
        .map_err(|error| AppError::Secret(error.to_string()))?
        .delete_credential()
    {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(AppError::Secret(error.to_string())),
    }
}

fn fallback_secret_path() -> Result<PathBuf, AppError> {
    let dirs = ProjectDirs::from("dev", "ai-playground", "ai-playground").ok_or_else(|| {
        AppError::Config {
            path: PathBuf::from("<unknown>"),
            message: "Could not resolve data directory".to_string(),
        }
    })?;
    Ok(dirs.data_local_dir().join("secrets.toon"))
}

fn load_fallback_secrets() -> Result<StoredSecrets, AppError> {
    let path = fallback_secret_path()?;
    if !path.exists() {
        return Ok(StoredSecrets::default());
    }
    let raw = fs::read_to_string(&path).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("read failed: {error}"),
    })?;
    crate::toon_codec::from_str_or_json::<StoredSecrets>(&raw)
}

fn save_fallback_secrets(secrets: &StoredSecrets) -> Result<(), AppError> {
    let path = fallback_secret_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::Config {
            path: parent.to_path_buf(),
            message: format!("could not create directory: {error}"),
        })?;
    }
    let raw = crate::toon_codec::to_string(secrets)?;
    fs::write(&path, raw).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("write failed: {error}"),
    })?;
    set_owner_read_write(&path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_read_write(path: &PathBuf) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, permissions).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("could not set secret file permissions: {error}"),
    })
}

#[cfg(not(unix))]
fn set_owner_read_write(_path: &PathBuf) -> Result<(), AppError> {
    Ok(())
}

fn set_fallback_token(token_ref: &str, token: &str) -> Result<(), AppError> {
    let mut secrets = load_fallback_secrets()?;
    secrets
        .tokens
        .insert(token_ref.to_string(), token.to_string());
    save_fallback_secrets(&secrets)
}

fn get_fallback_token(token_ref: &str) -> Result<Option<String>, AppError> {
    Ok(load_fallback_secrets()?.tokens.get(token_ref).cloned())
}

fn delete_fallback_token(token_ref: &str) -> Result<(), AppError> {
    let mut secrets = load_fallback_secrets()?;
    secrets.tokens.remove(token_ref);
    save_fallback_secrets(&secrets)
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
