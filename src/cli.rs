use std::io::{self, Write};

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    chat,
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        ProviderClient, ProviderKind, ReqwestProviderClient, ResponseControl, ResponseFormat,
        validate_base_url,
    },
    secrets::{KeyringSecretStore, SecretStore},
};

#[derive(Debug, Parser)]
#[command(name = "aiteach", version, about = "Rust CLI for LLM chat")]
pub struct Cli {
    #[arg(long, global = true)]
    verbose: bool,
    #[arg(long, global = true)]
    log_conversation: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    Ask(AskArgs),
    Chat(ProfileArg),
    #[command(about = "Run the same prompt once without controls and once with response controls")]
    Compare(CompareArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Doctor(ProfileArg),
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    Add(ProfileAddArgs),
    List,
    Use { name: String },
    Remove { name: String },
}

#[derive(Debug, Args)]
struct ProfileAddArgs {
    name: String,
    #[arg(long)]
    provider: ProviderKind,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    Set(ProfileArg),
    Delete(ProfileArg),
}

#[derive(Debug, Subcommand)]
enum ModelsCommand {
    List(ProfileArg),
}

#[derive(Debug, Args)]
struct AskArgs {
    prompt: String,
    #[arg(long)]
    profile: Option<String>,
    #[command(flatten)]
    control: ResponseControlArgs,
}

#[derive(Debug, Args)]
struct CompareArgs {
    prompt: String,
    #[arg(long)]
    profile: Option<String>,
    #[command(flatten)]
    control: ResponseControlArgs,
}

#[derive(Debug, Args)]
struct ProfileArg {
    #[arg(long)]
    profile: Option<String>,
    #[command(flatten)]
    control: ResponseControlArgs,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Path,
}

#[derive(Debug, Clone, Args, Default)]
struct ResponseControlArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = CliResponseFormat::Text,
        help = "Response format requested from the provider"
    )]
    response_format: CliResponseFormat,
    #[arg(
        long,
        help = "Maximum number of output tokens requested from the provider"
    )]
    max_tokens: Option<u32>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Stop sequence; can be provided multiple times"
    )]
    stop: Vec<String>,
    #[arg(
        long,
        help = "System instruction that explicitly describes the response format"
    )]
    format_instruction: Option<String>,
    #[arg(
        long,
        help = "System instruction that describes when the answer should finish"
    )]
    completion_instruction: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum CliResponseFormat {
    #[default]
    Text,
    JsonObject,
}

impl From<&ResponseControlArgs> for ResponseControl {
    fn from(args: &ResponseControlArgs) -> Self {
        Self {
            format: match args.response_format {
                CliResponseFormat::Text => ResponseFormat::Text,
                CliResponseFormat::JsonObject => ResponseFormat::JsonObject,
            },
            max_tokens: args.max_tokens,
            stop: args.stop.clone(),
            format_instruction: args.format_instruction.clone(),
            completion_instruction: args.completion_instruction.clone(),
        }
    }
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let result = run_with_store(&cli, &KeyringSecretStore).await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if cli.verbose {
                eprintln!("{error:?}");
            } else {
                eprintln!("{error}");
            }
            Err(anyhow::anyhow!(error))
        }
    }
}

async fn run_with_store(cli: &Cli, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let _ = cli.log_conversation;
    match &cli.command {
        Command::Profile { command } => profile_command(command, secrets),
        Command::Token { command } => token_command(command, secrets),
        Command::Models { command } => models_command(command, secrets).await,
        Command::Ask(args) => ask_command(args, secrets).await,
        Command::Chat(args) => chat_command(args, secrets).await,
        Command::Compare(args) => compare_command(args, secrets).await,
        Command::Config { command } => match command {
            ConfigCommand::Path => {
                println!("{}", AppConfig::config_path()?.display());
                Ok(())
            }
        },
        Command::Doctor(args) => doctor_command(args, secrets),
    }
}

fn profile_command(command: &ProfileCommand, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    match command {
        ProfileCommand::Add(args) => {
            let base_url = args
                .base_url
                .clone()
                .unwrap_or_else(|| args.provider.default_base_url().to_string());
            validate_base_url(&args.name, &base_url)?;
            config.add_profile(
                args.name.clone(),
                ProfileConfig {
                    provider: args.provider,
                    model: args
                        .model
                        .clone()
                        .unwrap_or_else(|| args.provider.default_model().to_string()),
                    base_url,
                    token_ref: String::new(),
                },
            );
            config.save()?;
            println!("Profile '{}' added.", args.name);
        }
        ProfileCommand::List => {
            for (name, profile) in &config.profiles {
                let active = if config.active_profile.as_deref() == Some(name) {
                    "*"
                } else {
                    " "
                };
                let has_token = secrets.get_token(&profile.token_ref)?.is_some();
                let token = if has_token {
                    "token: present"
                } else {
                    "token: missing"
                };
                println!(
                    "{active} {name} [{}] {} {token}",
                    profile.provider, profile.model
                );
            }
        }
        ProfileCommand::Use { name } => {
            config.use_profile(name)?;
            config.save()?;
            println!("Active profile: {name}");
        }
        ProfileCommand::Remove { name } => {
            let removed = config.remove_profile(name)?;
            secrets.delete_token(&removed.token_ref)?;
            config.save()?;
            println!("Profile '{name}' removed.");
        }
    }
    Ok(())
}

fn token_command(command: &TokenCommand, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    match command {
        TokenCommand::Set(args) => {
            let (name, profile) = config.selected_profile(args.profile.as_deref())?;
            eprint!("Token for profile '{name}': ");
            io::stderr()
                .flush()
                .map_err(|error| AppError::Secret(error.to_string()))?;
            let mut token = String::new();
            io::stdin()
                .read_line(&mut token)
                .map_err(|error| AppError::Secret(error.to_string()))?;
            secrets.set_token(&profile.token_ref, token.trim())?;
            println!("Token saved for profile '{name}'.");
        }
        TokenCommand::Delete(args) => {
            let (name, profile) = config.selected_profile(args.profile.as_deref())?;
            secrets.delete_token(&profile.token_ref)?;
            println!("Token deleted for profile '{name}'.");
        }
    }
    Ok(())
}

async fn models_command(
    command: &ModelsCommand,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let ModelsCommand::List(args) = command;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let token = secrets
        .get_token(&profile.token_ref)?
        .ok_or_else(|| AppError::MissingToken {
            profile: name.to_string(),
        })?;
    eprintln!("Waiting for provider model list...");
    let client = ReqwestProviderClient::new()?;
    for model in client.list_models(profile, &token).await? {
        println!("{model}");
    }
    Ok(())
}

async fn ask_command(args: &AskArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    eprintln!("Waiting for provider response...");
    let client = ReqwestProviderClient::new()?;
    let text = chat::ask_once(
        &client,
        secrets,
        &name,
        profile,
        args.prompt.clone(),
        ResponseControl::from(&args.control),
    )
    .await?;
    println!("{text}");
    Ok(())
}

async fn chat_command(args: &ProfileArg, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let client = ReqwestProviderClient::new()?;
    chat::interactive_chat(
        &client,
        secrets,
        &config,
        &name,
        profile,
        ResponseControl::from(&args.control),
    )
    .await
}

async fn compare_command(args: &CompareArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let control = ResponseControl::from(&args.control);
    eprintln!("Waiting for unrestricted and controlled provider responses...");
    let client = ReqwestProviderClient::new()?;
    let (unrestricted, controlled) = chat::compare_response_control(
        &client,
        secrets,
        &name,
        profile,
        args.prompt.clone(),
        control,
    )
    .await?;
    println!("## Without constraints\n{unrestricted}\n");
    println!("## With constraints\n{controlled}");
    Ok(())
}

fn doctor_command(args: &ProfileArg, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let path = AppConfig::config_path()?;
    println!("Config path: {}", path.display());
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    validate_base_url(&name, &profile.base_url)?;
    println!("Profile: {name}");
    println!("Provider: {}", profile.provider);
    println!("Model: {}", profile.model);
    println!("Base URL: valid");
    let token_present = secrets.get_token(&profile.token_ref)?.is_some();
    println!(
        "Token: {}",
        if token_present { "present" } else { "missing" }
    );
    Ok(())
}
