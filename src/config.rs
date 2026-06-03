use std::{collections::BTreeMap, fs, path::PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{errors::AppError, providers::ProviderKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileConfig {
    pub provider: ProviderKind,
    pub model: String,
    pub base_url: String,
    pub token_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AppConfig {
    pub active_profile: Option<String>,
    pub profiles: BTreeMap<String, ProfileConfig>,
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf, AppError> {
        let dirs =
            ProjectDirs::from("dev", "aiteach", "aiteach").ok_or_else(|| AppError::Config {
                path: PathBuf::from("<unknown>"),
                message: "Could not resolve OS config directory".to_string(),
            })?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load() -> Result<Self, AppError> {
        Self::load_from_path(Self::config_path()?)
    }

    pub fn load_from_path(path: PathBuf) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path).map_err(|error| AppError::Config {
            path: path.clone(),
            message: format!("read failed: {error}"),
        })?;
        toml::from_str(&raw).map_err(|error| AppError::Config {
            path,
            message: format!("invalid TOML: {error}"),
        })
    }

    pub fn save(&self) -> Result<(), AppError> {
        self.save_to_path(Self::config_path()?)
    }

    pub fn save_to_path(&self, path: PathBuf) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| AppError::Config {
                path: path.clone(),
                message: format!("could not create directory: {error}"),
            })?;
        }
        let raw = toml::to_string_pretty(self).map_err(|error| AppError::Config {
            path: path.clone(),
            message: format!("could not serialize config: {error}"),
        })?;
        fs::write(&path, raw).map_err(|error| AppError::Config {
            path,
            message: format!("write failed: {error}"),
        })
    }

    pub fn add_profile(&mut self, name: String, mut profile: ProfileConfig) {
        profile.token_ref = token_ref(&profile.provider);
        self.profiles.insert(name.clone(), profile);
        if self.active_profile.is_none() {
            self.active_profile = Some(name);
        }
    }

    pub fn use_profile(&mut self, name: &str) -> Result<(), AppError> {
        if !self.profiles.contains_key(name) {
            return Err(AppError::ProfileMissing(name.to_string()));
        }
        self.active_profile = Some(name.to_string());
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<ProfileConfig, AppError> {
        let removed = self
            .profiles
            .remove(name)
            .ok_or_else(|| AppError::ProfileMissing(name.to_string()))?;
        if self.active_profile.as_deref() == Some(name) {
            self.active_profile = self.profiles.keys().next().cloned();
        }
        Ok(removed)
    }

    pub fn selected_profile(
        &self,
        requested: Option<&str>,
    ) -> Result<(String, &ProfileConfig), AppError> {
        let name = match requested {
            Some(value) => value.to_string(),
            None => self
                .active_profile
                .clone()
                .ok_or(AppError::NoActiveProfile)?,
        };
        let profile = self
            .profiles
            .get(&name)
            .ok_or_else(|| AppError::ProfileMissing(name.clone()))?;
        Ok((name, profile))
    }
}

pub fn token_ref(provider: &ProviderKind) -> String {
    provider.to_string()
}

pub fn legacy_token_ref(provider: &ProviderKind, profile_name: &str) -> String {
    format!("{provider}:{profile_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_load_save_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.add_profile(
            "work".to_string(),
            ProfileConfig {
                provider: ProviderKind::OpenRouter,
                model: "openai/gpt-4.1".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                token_ref: String::new(),
            },
        );

        config.save_to_path(path.clone()).expect("save");
        let loaded = AppConfig::load_from_path(path).expect("load");

        assert_eq!(loaded, config);
        assert_eq!(loaded.profiles["work"].token_ref, "openrouter".to_string());
    }

    #[test]
    fn legacy_token_refs_keep_profile_name_for_migration() {
        assert_eq!(
            legacy_token_ref(&ProviderKind::DeepSeek, "work"),
            "deepseek:work"
        );
    }

    #[test]
    fn profile_selection_uses_active_or_requested() {
        let mut config = AppConfig::default();
        config.add_profile(
            "one".to_string(),
            ProfileConfig {
                provider: ProviderKind::DeepSeek,
                model: "deepseek-chat".to_string(),
                base_url: ProviderKind::DeepSeek.default_base_url().to_string(),
                token_ref: String::new(),
            },
        );
        config.add_profile(
            "two".to_string(),
            ProfileConfig {
                provider: ProviderKind::Kimi,
                model: "moonshot-v1-8k".to_string(),
                base_url: ProviderKind::Kimi.default_base_url().to_string(),
                token_ref: String::new(),
            },
        );

        assert_eq!(config.selected_profile(None).expect("active").0, "one");
        assert_eq!(
            config.selected_profile(Some("two")).expect("requested").0,
            "two"
        );
        assert!(matches!(
            config.selected_profile(Some("missing")),
            Err(AppError::ProfileMissing(_))
        ));
    }
}
