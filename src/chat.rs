use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use directories::ProjectDirs;

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{ChatMessage, ChatRequest, ProviderClient, Role},
    secrets::SecretStore,
};

pub async fn ask_once(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
) -> Result<String, AppError> {
    let token = secrets
        .get_token(&profile.token_ref)?
        .ok_or_else(|| AppError::MissingToken {
            profile: profile_name.to_string(),
        })?;
    let response = client
        .chat_completion(
            profile,
            &token,
            ChatRequest {
                model: profile.model.clone(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: prompt,
                }],
            },
        )
        .await?;
    Ok(response.text)
}

pub async fn interactive_chat(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<(), AppError> {
    let token = secrets
        .get_token(&profile.token_ref)?
        .ok_or_else(|| AppError::MissingToken {
            profile: profile_name.to_string(),
        })?;
    let mut messages = Vec::<ChatMessage>::new();
    println!(
        "Chat started with profile '{profile_name}' and model '{}'.",
        profile.model
    );
    println!("Use /exit, /profile, /model, /clear, /save.");

    loop {
        print!("> ");
        io::stdout()
            .flush()
            .map_err(|error| AppError::Secret(error.to_string()))?;
        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .map_err(|error| AppError::Secret(error.to_string()))?;
        if read == 0 {
            break;
        }
        let line = line.trim();
        match line {
            "" => continue,
            "/exit" => break,
            "/profile" => {
                println!("{profile_name}");
                continue;
            }
            "/model" => {
                println!("{}", profile.model);
                continue;
            }
            "/clear" => {
                messages.clear();
                println!("History cleared.");
                continue;
            }
            "/save" => {
                let path = save_history(profile_name, &messages)?;
                println!("Saved to {}", path.display());
                continue;
            }
            _ => {}
        }

        messages.push(ChatMessage {
            role: Role::User,
            content: line.to_string(),
        });
        eprintln!("Waiting for provider response...");
        let response = client
            .chat_completion(
                profile,
                &token,
                ChatRequest {
                    model: profile.model.clone(),
                    messages: messages.clone(),
                },
            )
            .await?;
        println!("{}", response.text);
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: response.text,
        });
    }

    if config.active_profile.as_deref() != Some(profile_name) {
        println!("Session used non-active profile '{profile_name}'.");
    }
    Ok(())
}

pub fn save_history(profile_name: &str, messages: &[ChatMessage]) -> Result<PathBuf, AppError> {
    let dirs = ProjectDirs::from("dev", "aiteach", "aiteach").ok_or_else(|| AppError::Config {
        path: PathBuf::from("<unknown>"),
        message: "Could not resolve data directory".to_string(),
    })?;
    let dir = dirs.data_local_dir().join("history");
    fs::create_dir_all(&dir).map_err(|error| AppError::Config {
        path: dir.clone(),
        message: format!("could not create history directory: {error}"),
    })?;
    let filename = format!(
        "{}-{}.json",
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
    let raw = serde_json::to_string_pretty(messages)
        .map_err(|error| AppError::Json(error.to_string()))?;
    fs::write(&path, raw).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("could not write history: {error}"),
    })?;
    Ok(path)
}
