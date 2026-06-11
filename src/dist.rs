use std::{env, fs, path::PathBuf};

use directories::ProjectDirs;
use reqwest::Client;

use crate::errors::AppError;

pub const DEFAULT_REPO_URL: &str = "https://github.com/Langepas94/ai-playground";
pub const DEFAULT_CHANNEL: &str = "dev";

#[derive(Debug, Clone)]
pub struct DistConfig {
    pub repo_url: String,
    pub channel: String,
    pub asset_name: String,
    pub install_path: PathBuf,
    pub url_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistStatus {
    pub install_path: PathBuf,
    pub download_url: String,
    pub installed: bool,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistInstallResult {
    pub install_path: PathBuf,
    pub download_url: String,
    pub bytes_written: u64,
}

pub fn default_config(
    channel: Option<String>,
    install_dir: Option<PathBuf>,
    url: Option<String>,
) -> Result<DistConfig, AppError> {
    let repo_url =
        env::var("AI_PLAYGROUND_DIST_REPO_URL").unwrap_or_else(|_| DEFAULT_REPO_URL.to_string());
    let channel = channel
        .or_else(|| env::var("AI_PLAYGROUND_DIST_CHANNEL").ok())
        .unwrap_or_else(|| DEFAULT_CHANNEL.to_string());
    let asset_name = target_asset_name();
    let install_path = install_dir
        .unwrap_or_else(default_install_dir)
        .join(binary_filename());
    Ok(DistConfig {
        repo_url,
        channel,
        asset_name,
        install_path,
        url_override: url,
    })
}

pub fn status(config: &DistConfig) -> DistStatus {
    let installed = config.install_path.exists();
    let executable = is_executable(&config.install_path);
    DistStatus {
        install_path: config.install_path.clone(),
        download_url: download_url(config),
        installed,
        executable,
    }
}

pub async fn install(
    client: &Client,
    config: &DistConfig,
    overwrite: bool,
) -> Result<DistInstallResult, AppError> {
    if config.install_path.exists() && !overwrite {
        return Err(AppError::InvalidInput(format!(
            "Binary already exists at {}. Use --overwrite to replace it.",
            config.install_path.display()
        )));
    }

    let response = client
        .get(download_url(config))
        .send()
        .await
        .map_err(AppError::from)?
        .error_for_status()
        .map_err(AppError::from)?;
    let bytes = response.bytes().await.map_err(AppError::from)?;

    if let Some(parent) = config.install_path.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::Config {
            path: parent.to_path_buf(),
            message: format!("could not create install directory: {error}"),
        })?;
    }
    fs::write(&config.install_path, &bytes).map_err(|error| AppError::Config {
        path: config.install_path.clone(),
        message: format!("could not write binary: {error}"),
    })?;
    make_executable(&config.install_path)?;

    Ok(DistInstallResult {
        install_path: config.install_path.clone(),
        download_url: download_url(config),
        bytes_written: bytes.len() as u64,
    })
}

pub fn download_url(config: &DistConfig) -> String {
    if let Some(url) = &config.url_override {
        return url.clone();
    }
    format!(
        "{}/releases/download/{}/{}",
        config.repo_url.trim_end_matches('/'),
        config.channel,
        config.asset_name
    )
}

fn default_install_dir() -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(".local").join("bin");
    }
    PathBuf::from(".")
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

fn target_asset_name() -> String {
    let mut name = format!("ai-{}-{}", env::consts::OS, env::consts::ARCH);
    if cfg!(target_os = "windows") {
        name.push_str(".exe");
    }
    name
}

fn binary_filename() -> String {
    if cfg!(target_os = "windows") {
        "ai.exe".to_string()
    } else {
        "ai".to_string()
    }
}

fn is_executable(path: &PathBuf) -> bool {
    if !path.exists() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn make_executable(path: &PathBuf) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|error| AppError::Config {
                path: path.clone(),
                message: format!("could not read installed binary metadata: {error}"),
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|error| AppError::Config {
            path: path.clone(),
            message: format!("could not set executable bit: {error}"),
        })?;
    }
    Ok(())
}

pub fn current_binary_path() -> Option<PathBuf> {
    env::current_exe().ok()
}

pub fn default_release_note() -> String {
    let repo =
        env::var("AI_PLAYGROUND_DIST_REPO_URL").unwrap_or_else(|_| DEFAULT_REPO_URL.to_string());
    format!(
        "Downloadable dev build: {repo}/releases/download/{DEFAULT_CHANNEL}/{}",
        target_asset_name()
    )
}

pub fn suggested_install_dir() -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(".local").join("bin");
    }
    ProjectDirs::from("dev", "ai-playground", "ai-playground")
        .map(|dirs| dirs.data_local_dir().join("bin"))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_default_github_release_url() {
        let config = DistConfig {
            repo_url: "https://github.com/Langepas94/ai-playground".to_string(),
            channel: "dev".to_string(),
            asset_name: "ai-linux-x86_64".to_string(),
            install_path: PathBuf::from("/tmp/ai"),
            url_override: None,
        };

        assert_eq!(
            download_url(&config),
            "https://github.com/Langepas94/ai-playground/releases/download/dev/ai-linux-x86_64"
        );
    }

    #[test]
    fn override_url_wins_over_release_template() {
        let config = DistConfig {
            repo_url: "https://github.com/Langepas94/ai-playground".to_string(),
            channel: "dev".to_string(),
            asset_name: "ai-linux-x86_64".to_string(),
            install_path: PathBuf::from("/tmp/ai"),
            url_override: Some("https://example.test/ai".to_string()),
        };

        assert_eq!(download_url(&config), "https://example.test/ai");
    }

    #[test]
    fn suggested_install_dir_has_bin_suffix() {
        assert!(
            suggested_install_dir().ends_with(".local/bin")
                || suggested_install_dir().ends_with("bin")
        );
    }
}
