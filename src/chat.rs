use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use directories::ProjectDirs;

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{ChatMessage, ChatRequest, ProviderClient, ResponseControl, ResponseFormat, Role},
    secrets::SecretStore,
};

pub async fn ask_once(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    control: ResponseControl,
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
                control,
            },
        )
        .await?;
    Ok(response.text)
}

pub async fn compare_response_control(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    controlled: ResponseControl,
) -> Result<(String, String), AppError> {
    let token = secrets
        .get_token(&profile.token_ref)?
        .ok_or_else(|| AppError::MissingToken {
            profile: profile_name.to_string(),
        })?;
    let base_request = ChatRequest {
        model: profile.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt.clone(),
        }],
        control: ResponseControl::uncontrolled(),
    };
    let controlled_request = ChatRequest {
        model: profile.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt,
        }],
        control: controlled,
    };
    let unrestricted = client
        .chat_completion(profile, &token, base_request)
        .await?
        .text;
    let restricted = client
        .chat_completion(profile, &token, controlled_request)
        .await?
        .text;
    Ok((unrestricted, restricted))
}

pub async fn interactive_chat(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    mut control: ResponseControl,
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
    println!("Use /exit, /profile, /model, /clear, /save, /control, /format, /max-tokens, /stop.");

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
            "/control" => {
                println!("{}", describe_control(&control));
                continue;
            }
            "/control clear" => {
                control = ResponseControl::uncontrolled();
                println!("Response control cleared.");
                continue;
            }
            "/stop clear" => {
                control.stop.clear();
                println!("Stop sequences cleared.");
                continue;
            }
            _ => {}
        }

        if let Some(value) = line.strip_prefix("/format ") {
            match value {
                "text" => {
                    control.format = ResponseFormat::Text;
                    println!("Response format: text");
                }
                "json" | "json-object" => {
                    control.format = ResponseFormat::JsonObject;
                    println!("Response format: json-object");
                }
                _ => println!("Use /format text or /format json-object."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/max-tokens ") {
            match value.parse::<u32>() {
                Ok(max_tokens) => {
                    control.max_tokens = Some(max_tokens);
                    println!("Max tokens: {max_tokens}");
                }
                Err(_) => println!("Use /max-tokens <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/stop ") {
            if value.is_empty() {
                println!("Use /stop <sequence> or /stop clear.");
            } else {
                control.stop.push(value.to_string());
                println!("Stop sequence added.");
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/completion-instruction ") {
            control.completion_instruction = Some(value.to_string());
            println!("Completion instruction updated.");
            continue;
        }

        if let Some(value) = line.strip_prefix("/format-instruction ") {
            control.format_instruction = Some(value.to_string());
            println!("Format instruction updated.");
            continue;
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
                    control: control.clone(),
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

pub fn describe_control(control: &ResponseControl) -> String {
    if control.is_uncontrolled() {
        return "Response control: none".to_string();
    }
    let stop = if control.stop.is_empty() {
        "none".to_string()
    } else {
        control.stop.join(", ")
    };
    format!(
        "Response control: format={}, max_tokens={}, stop={}, format_instruction={}, completion_instruction={}",
        control.format,
        control
            .max_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        stop,
        control.format_instruction.as_deref().unwrap_or("none"),
        control.completion_instruction.as_deref().unwrap_or("none")
    )
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
