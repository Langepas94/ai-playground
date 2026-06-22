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

/// A configured MCP server the client can connect to. Either a local stdio
/// child process or a remote streamable-HTTP endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        url: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub active_profile: Option<String>,
    pub profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default = "default_mcp_servers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_profile: None,
            profiles: BTreeMap::new(),
            mcp_servers: default_mcp_servers(),
        }
    }
}

/// A sane default server so the inspector is usable out of the box: the
/// reference filesystem MCP server launched via `npx`, scoped to the current
/// directory.
fn default_mcp_servers() -> BTreeMap<String, McpServerConfig> {
    let mut servers = BTreeMap::new();
    servers.insert(
        "filesystem".to_string(),
        McpServerConfig::Stdio {
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                ".".to_string(),
            ],
        },
    );
    servers
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf, AppError> {
        project_config_path("ai-playground", "ai-playground")
    }

    fn legacy_config_path() -> Result<PathBuf, AppError> {
        project_config_path("aiteach", "aiteach")
    }

    pub fn existing_config_path() -> Result<PathBuf, AppError> {
        let path = Self::config_path()?;
        if path.exists() {
            return Ok(path);
        }
        let legacy_path = Self::legacy_config_path()?;
        if legacy_path.exists() {
            return Ok(legacy_path);
        }
        Ok(path)
    }

    pub fn load() -> Result<Self, AppError> {
        Self::load_from_path(Self::existing_config_path()?)
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

    pub fn mcp_server(&self, name: &str) -> Result<&McpServerConfig, AppError> {
        self.mcp_servers
            .get(name)
            .ok_or_else(|| AppError::Mcp(format!("MCP server '{name}' is not configured")))
    }
}

fn project_config_path(qualifier: &str, application: &str) -> Result<PathBuf, AppError> {
    let dirs =
        ProjectDirs::from("dev", qualifier, application).ok_or_else(|| AppError::Config {
            path: PathBuf::from("<unknown>"),
            message: "Could not resolve OS config directory".to_string(),
        })?;
    Ok(dirs.config_dir().join("config.toml"))
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
    fn default_config_includes_filesystem_mcp_server() {
        let config = AppConfig::default();
        let server = config
            .mcp_servers
            .get("filesystem")
            .expect("default filesystem server");
        match server {
            McpServerConfig::Stdio { command, args } => {
                assert_eq!(command, "npx");
                assert!(args.iter().any(|arg| arg.contains("server-filesystem")));
            }
            other => panic!("expected stdio server, got {other:?}"),
        }
    }

    #[test]
    fn mcp_server_lookup_ok_and_missing() {
        let config = AppConfig::default();
        assert!(config.mcp_server("filesystem").is_ok());
        let error = config.mcp_server("absent").expect_err("missing server");
        assert!(matches!(error, AppError::Mcp(_)));
    }

    #[test]
    fn config_roundtrip_preserves_stdio_and_http_servers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.mcp_servers.insert(
            "remote".to_string(),
            McpServerConfig::Http {
                url: "https://example.test/mcp".to_string(),
            },
        );
        config.mcp_servers.insert(
            "local".to_string(),
            McpServerConfig::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "some-server".to_string()],
            },
        );

        config.save_to_path(path.clone()).expect("save");
        let loaded = AppConfig::load_from_path(path).expect("load");

        assert_eq!(loaded, config);
        assert!(matches!(
            loaded.mcp_servers["remote"],
            McpServerConfig::Http { .. }
        ));
    }

    #[test]
    fn config_without_mcp_servers_section_gets_default_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let raw = "active_profile = \"work\"\n\
            [profiles.work]\n\
            provider = \"open-router\"\n\
            model = \"m\"\n\
            base_url = \"https://openrouter.ai/api/v1\"\n\
            token_ref = \"openrouter\"\n";
        fs::write(&path, raw).expect("write config");

        let loaded = AppConfig::load_from_path(path).expect("load");
        assert!(loaded.mcp_servers.contains_key("filesystem"));
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
