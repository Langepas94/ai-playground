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
    #[command(about = "Interactive first-run setup")]
    Setup(SetupArgs),
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
    Chat(ChatArgs),
    #[command(about = "Run the same prompt once without controls and once with response controls")]
    Compare(CompareArgs),
    #[command(about = "Compare state-based, instruction-based, and combined dialogue stopping")]
    CompareGoal(CompareGoalArgs),
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
    name: Option<String>,
    #[arg(long)]
    provider: Option<ProviderKind>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    provider: Option<ProviderKind>,
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
struct CompareGoalArgs {
    prompt: String,
    #[arg(long)]
    profile: Option<String>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Required entity field; provide at least one"
    )]
    required_field: Vec<String>,
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[arg(long)]
    profile: Option<String>,
    #[command(flatten)]
    control: ResponseControlArgs,
    #[command(flatten)]
    goal: ConversationGoalArgs,
}

#[derive(Debug, Args)]
struct ProfileArg {
    #[arg(long)]
    profile: Option<String>,
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

#[derive(Debug, Clone, Args)]
struct ConversationGoalArgs {
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Required field for stateful dialogue completion; can be provided multiple times"
    )]
    required_field: Vec<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = CliConversationStopMode::Manual,
        help = "How chat decides that the dialogue goal is complete"
    )]
    goal_stop_mode: CliConversationStopMode,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum CliResponseFormat {
    #[default]
    Text,
    JsonObject,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum CliConversationStopMode {
    #[default]
    Manual,
    State,
    Instruction,
    Combined,
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

impl From<&ConversationGoalArgs> for chat::ConversationGoal {
    fn from(args: &ConversationGoalArgs) -> Self {
        Self {
            required_fields: args.required_field.clone(),
            mode: match args.goal_stop_mode {
                CliConversationStopMode::Manual => chat::ConversationStopMode::Manual,
                CliConversationStopMode::State => chat::ConversationStopMode::State,
                CliConversationStopMode::Instruction => chat::ConversationStopMode::Instruction,
                CliConversationStopMode::Combined => chat::ConversationStopMode::Combined,
            },
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
        Command::Setup(args) => setup_command(args, secrets),
        Command::Profile { command } => profile_command(command, secrets),
        Command::Token { command } => token_command(command, secrets),
        Command::Models { command } => models_command(command, secrets).await,
        Command::Ask(args) => ask_command(args, secrets).await,
        Command::Chat(args) => chat_command(args, secrets).await,
        Command::Compare(args) => compare_command(args, secrets).await,
        Command::CompareGoal(args) => compare_goal_command(args, secrets).await,
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
            let profile = collect_profile_input(
                args.name.clone(),
                args.provider,
                args.model.clone(),
                args.base_url.clone(),
            )?;
            validate_base_url(&profile.name, &profile.config.base_url)?;
            config.add_profile(profile.name.clone(), profile.config);
            config.save()?;
            println!("Profile '{}' added.", profile.name);
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

fn setup_command(args: &SetupArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    let profile = collect_profile_input(
        args.name.clone(),
        args.provider,
        args.model.clone(),
        args.base_url.clone(),
    )?;
    validate_base_url(&profile.name, &profile.config.base_url)?;
    config.add_profile(profile.name.clone(), profile.config);
    config.use_profile(&profile.name)?;
    let token_ref = config.profiles[&profile.name].token_ref.clone();
    config.save()?;

    println!("Profile '{}' is active.", profile.name);
    let token = prompt_optional_secret("Paste API token now, or press Enter to skip")?;
    if let Some(token) = token {
        secrets.set_token(&token_ref, &token)?;
        println!("Token saved for profile '{}'.", profile.name);
    } else {
        println!(
            "Token skipped. Add it later with `aiteach token set --profile {}`.",
            profile.name
        );
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

#[derive(Debug, Clone)]
struct CollectedProfile {
    name: String,
    config: ProfileConfig,
}

fn collect_profile_input(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
) -> Result<CollectedProfile, AppError> {
    let provider = match provider {
        Some(provider) => provider,
        None => prompt_provider()?,
    };
    let name = match name {
        Some(name) => name,
        None => prompt_with_default("Profile name", &provider.to_string())?,
    };
    let model = match model {
        Some(model) => model,
        None => prompt_with_default("Model", provider.default_model())?,
    };
    let base_url = match base_url {
        Some(base_url) => base_url,
        None => prompt_with_default("Base URL", provider.default_base_url())?,
    };

    Ok(CollectedProfile {
        name,
        config: ProfileConfig {
            provider,
            model,
            base_url,
            token_ref: String::new(),
        },
    })
}

fn prompt_provider() -> Result<ProviderKind, AppError> {
    println!("Choose provider:");
    for (index, provider) in ProviderKind::all().iter().enumerate() {
        let spec = provider.spec();
        println!(
            "  {}. {} ({}) - default model: {}",
            index + 1,
            spec.display_name,
            provider,
            spec.default_model
        );
    }

    loop {
        let raw = prompt("Provider number or name")?;
        if raw.trim().is_empty() {
            return Ok(ProviderKind::OpenRouter);
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(provider) = ProviderKind::all().get(index.saturating_sub(1))
        {
            return Ok(*provider);
        }
        if let Ok(provider) = raw.parse::<ProviderKind>() {
            return Ok(provider);
        }
        println!("Unknown provider. Choose a number from the list or type a provider name.");
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String, AppError> {
    let raw = prompt(&format!("{label} [{default}]"))?;
    if raw.trim().is_empty() {
        Ok(default.to_string())
    } else {
        Ok(raw)
    }
}

fn prompt(label: &str) -> Result<String, AppError> {
    print!("{label}: ");
    io::stdout()
        .flush()
        .map_err(|error| AppError::Secret(error.to_string()))?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| AppError::Secret(error.to_string()))?;
    Ok(value.trim().to_string())
}

fn prompt_optional_secret(label: &str) -> Result<Option<String>, AppError> {
    let value = prompt(label)?;
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
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

async fn chat_command(args: &ChatArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
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
        chat::ConversationGoal::from(&args.goal),
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

async fn compare_goal_command(
    args: &CompareGoalArgs,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    if args.required_field.is_empty() {
        return Err(AppError::InvalidInput(
            "compare-goal requires at least one --required-field".to_string(),
        ));
    }
    eprintln!("Waiting for state, instruction, and combined goal-stop responses...");
    let client = ReqwestProviderClient::new()?;
    let comparison = chat::compare_goal_stop(
        &client,
        secrets,
        &name,
        profile,
        args.prompt.clone(),
        args.required_field.clone(),
    )
    .await?;
    print_goal_run("State-based stop", &comparison.state);
    print_goal_run("Instruction-based stop", &comparison.instruction);
    print_goal_run("Combined stop", &comparison.combined);
    Ok(())
}

fn print_goal_run(title: &str, run: &chat::GoalRun) {
    println!(
        "## {title}\nmode: {}\nstopped: {}\nstate: {}\n{}",
        run.mode, run.stopped, run.state_summary, run.response
    );
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
