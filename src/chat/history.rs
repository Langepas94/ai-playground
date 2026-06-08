use std::{fs, path::PathBuf};

use directories::ProjectDirs;

use crate::{errors::AppError, providers::ChatMessage};

pub fn save_history(profile_name: &str, messages: &[ChatMessage]) -> Result<PathBuf, AppError> {
    let dirs = ProjectDirs::from("dev", "ai-playground", "ai-playground").ok_or_else(|| {
        AppError::Config {
            path: PathBuf::from("<unknown>"),
            message: "Could not resolve data directory".to_string(),
        }
    })?;
    let dir = dirs.data_local_dir().join("history");
    fs::create_dir_all(&dir).map_err(|error| AppError::Config {
        path: dir.clone(),
        message: format!("could not create history directory: {error}"),
    })?;
    let filename = format!(
        "{}-{}.toon",
        profile_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::Config {
                path: dir.clone(),
                message: error.to_string(),
            })?
            .as_secs()
    );
    let path = dir.join(filename);
    let raw = crate::toon_codec::to_string(messages)?;
    fs::write(&path, raw).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("could not write history: {error}"),
    })?;
    Ok(path)
}
