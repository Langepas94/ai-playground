use std::io::{self, BufRead, Write};
use std::net::SocketAddr;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    chat,
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        AnswerFormat, ProviderClient, ProviderKind, ReqwestProviderClient, ResponseControl,
        ResponseFormat, validate_base_url,
    },
    secrets::{
        KeyringSecretStore, SecretStore, delete_legacy_profile_token, delete_profile_token,
        get_config_profile_token, set_profile_token,
    },
};

#[derive(Debug, Parser)]
#[command(name = "ai-playground", version, about = "Rust CLI for LLM chat")]
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
    #[command(about = "Select active profile from a menu or by name")]
    Use(ProfileUseArgs),
    #[command(about = "Start the local web UI")]
    Web(WebArgs),
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
    Use(ProfileUseArgs),
    Remove {
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
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
struct ProfileUseArgs {
    #[arg(num_args = 0..)]
    name: Vec<String>,
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
struct WebArgs {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
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
        value_enum,
        default_value_t = CliAnswerFormat::Natural,
        help = "Human-facing answer shape, such as bullets, steps, or table"
    )]
    answer_format: CliAnswerFormat,
    #[arg(
        long,
        help = "Maximum number of output tokens requested from the provider"
    )]
    max_tokens: Option<u32>,
    #[arg(
        long,
        help = "Maximum number of completion tokens requested from newer providers"
    )]
    max_completion_tokens: Option<u32>,
    #[arg(long, help = "Sampling temperature requested from the provider")]
    temperature: Option<f32>,
    #[arg(
        long,
        help = "Nucleus sampling probability requested from the provider"
    )]
    top_p: Option<f32>,
    #[arg(long, help = "Top-k sampling requested from providers that support it")]
    top_k: Option<u32>,
    #[arg(long, help = "Min-p sampling requested from providers that support it")]
    min_p: Option<f32>,
    #[arg(long, help = "Top-a sampling requested from providers that support it")]
    top_a: Option<f32>,
    #[arg(long, help = "Presence penalty requested from the provider")]
    presence_penalty: Option<f32>,
    #[arg(long, help = "Frequency penalty requested from the provider")]
    frequency_penalty: Option<f32>,
    #[arg(
        long,
        help = "Repetition penalty requested from providers that support it"
    )]
    repetition_penalty: Option<f32>,
    #[arg(
        long,
        help = "Deterministic sampling seed, when supported by the provider"
    )]
    seed: Option<i64>,
    #[arg(
        long,
        help = "Reasoning effort, such as none, minimal, low, medium, high, or xhigh"
    )]
    reasoning_effort: Option<String>,
    #[arg(long, help = "Ask provider to include reasoning, when supported")]
    include_reasoning: Option<bool>,
    #[arg(long, help = "Verbosity, such as low, medium, or high")]
    verbosity: Option<String>,
    #[arg(long, help = "Request token log probabilities, when supported")]
    logprobs: Option<bool>,
    #[arg(long, help = "Number of top log probabilities to return")]
    top_logprobs: Option<u32>,
    #[arg(long, help = "Number of choices to generate")]
    n: Option<u32>,
    #[arg(long, help = "Whether provider may store this completion")]
    store: Option<bool>,
    #[arg(long, help = "Whether tools may be called in parallel")]
    parallel_tool_calls: Option<bool>,
    #[arg(long, help = "End-user identifier passed to the provider")]
    user: Option<String>,
    #[arg(long, help = "Service tier, such as auto, default, flex, or priority")]
    service_tier: Option<String>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Stop sequence; can be provided multiple times"
    )]
    stop: Vec<String>,
    #[arg(long, help = "Exact text the answer should start with")]
    answer_prefix: Option<String>,
    #[arg(long, help = "Exact text the answer should end with")]
    answer_suffix: Option<String>,
    #[arg(long, help = "Name or label the answer should address the user with")]
    address_as: Option<String>,
    #[arg(long, help = "Ask the model to quote the user's question first")]
    quote_question: bool,
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
enum CliAnswerFormat {
    #[default]
    Natural,
    Bullets,
    Numbered,
    Short,
    Steps,
    Table,
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
            answer_format: match args.answer_format {
                CliAnswerFormat::Natural => AnswerFormat::Natural,
                CliAnswerFormat::Bullets => AnswerFormat::Bullets,
                CliAnswerFormat::Numbered => AnswerFormat::Numbered,
                CliAnswerFormat::Short => AnswerFormat::Short,
                CliAnswerFormat::Steps => AnswerFormat::Steps,
                CliAnswerFormat::Table => AnswerFormat::Table,
            },
            max_tokens: args.max_tokens,
            max_completion_tokens: args.max_completion_tokens,
            temperature: args.temperature,
            top_p: args.top_p,
            top_k: args.top_k,
            min_p: args.min_p,
            top_a: args.top_a,
            presence_penalty: args.presence_penalty,
            frequency_penalty: args.frequency_penalty,
            repetition_penalty: args.repetition_penalty,
            seed: args.seed,
            reasoning_effort: args.reasoning_effort.clone(),
            include_reasoning: args.include_reasoning,
            verbosity: args.verbosity.clone(),
            logprobs: args.logprobs,
            top_logprobs: args.top_logprobs,
            n: args.n,
            store: args.store,
            parallel_tool_calls: args.parallel_tool_calls,
            user: args.user.clone(),
            service_tier: args.service_tier.clone(),
            extra_params: serde_json::Map::new(),
            stop: args.stop.clone(),
            answer_prefix: args.answer_prefix.clone(),
            answer_suffix: args.answer_suffix.clone(),
            address_as: args.address_as.clone(),
            quote_question: args.quote_question,
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
        Command::Setup(args) => setup_command(args, secrets).await,
        Command::Profile { command } => profile_command(command, secrets).await,
        Command::Token { command } => token_command(command, secrets),
        Command::Models { command } => models_command(command, secrets).await,
        Command::Ask(args) => ask_command(args, secrets).await,
        Command::Chat(args) => chat_command(args, secrets).await,
        Command::Use(args) => use_profile_command(args),
        Command::Web(args) => crate::web::serve(args.listen).await,
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

async fn profile_command(
    command: &ProfileCommand,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    match command {
        ProfileCommand::Add(args) => {
            let profile = collect_profile_input(
                args.name.clone(),
                args.provider,
                args.model.clone(),
                args.base_url.clone(),
                secrets,
                &config,
            )
            .await?;
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
                let has_token =
                    get_config_profile_token(secrets, &config, name, profile)?.is_some();
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
        ProfileCommand::Use(args) => {
            let name = select_profile_name(&config, &args.name)?;
            config.use_profile(&name)?;
            config.save()?;
            println!("Active profile: {name}");
        }
        ProfileCommand::Remove { name } => {
            let name = profile_name_from_parts(name);
            let removed = config.remove_profile(&name)?;
            delete_legacy_profile_token(secrets, &name, &removed)?;
            config.save()?;
            println!("Profile '{name}' removed.");
        }
    }
    Ok(())
}

fn profile_name_from_parts(parts: &[String]) -> String {
    parts.join(" ")
}

fn use_profile_command(args: &ProfileUseArgs) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    let name = select_profile_name(&config, &args.name)?;
    config.use_profile(&name)?;
    config.save()?;
    println!("Active profile: {name}");
    Ok(())
}

fn select_profile_name(config: &AppConfig, parts: &[String]) -> Result<String, AppError> {
    if !parts.is_empty() {
        return Ok(profile_name_from_parts(parts));
    }
    if config.profiles.is_empty() {
        return Err(AppError::InvalidInput(
            "No profiles found. Run `ai-playground setup` or `ai-playground profile add` first."
                .to_string(),
        ));
    }

    println!("Choose active profile.");
    for (index, (name, profile)) in config.profiles.iter().enumerate() {
        let active = if config.active_profile.as_deref() == Some(name) {
            " active"
        } else {
            ""
        };
        println!(
            "  {}. {} [{}] {}{}",
            index + 1,
            name,
            profile.provider,
            profile.model,
            active
        );
    }

    loop {
        let label = match config.active_profile.as_deref() {
            Some(active) => format!("Profile number or name [{active}]"),
            None => "Profile number or name".to_string(),
        };
        let raw = prompt(&label)?;
        if raw.trim().is_empty()
            && let Some(active) = config.active_profile.as_deref()
        {
            return Ok(active.to_string());
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(name) = config.profiles.keys().nth(index.saturating_sub(1))
        {
            return Ok(name.to_string());
        }
        if config.profiles.contains_key(raw.trim()) {
            return Ok(raw.trim().to_string());
        }
        println!("Choose a number from the list or type an existing profile name.");
    }
}

async fn setup_command(args: &SetupArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    println!("ai-playground setup");
    println!("Press Enter to accept the value shown in brackets.");
    println!();

    let mut config = AppConfig::load()?;
    let profile = collect_profile_input_with_optional_setup_token(
        args.name.clone(),
        args.provider,
        args.model.clone(),
        args.base_url.clone(),
        secrets,
        &config,
    )
    .await?;
    validate_base_url(&profile.name, &profile.config.base_url)?;
    let provider = profile.config.provider;
    config.add_profile(profile.name.clone(), profile.config);
    config.use_profile(&profile.name)?;
    config.save()?;

    println!();
    println!("Profile '{}' is active.", profile.name);
    if profile.token_saved {
        println!("Token saved for provider '{provider}'.");
    } else {
        println!(
            "Token skipped. Add it later with `ai-playground token set --profile {}`.",
            profile.name
        );
    }
    println!();
    println!("Next commands:");
    println!("  ai-playground doctor");
    println!("  ai-playground models list");
    println!("  ai-playground ask \"Сколько будет 3 + 2?\"");
    println!("  ai-playground chat");
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
                .map_err(|error| AppError::Terminal(error.to_string()))?;
            let token = read_stdin_line()?.unwrap_or_default();
            set_profile_token(secrets, profile, token.trim())?;
            println!("Token saved for provider '{}'.", profile.provider);
        }
        TokenCommand::Delete(args) => {
            let (name, profile) = config.selected_profile(args.profile.as_deref())?;
            delete_profile_token(secrets, &name, profile)?;
            println!("Token deleted for provider '{}'.", profile.provider);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct CollectedProfile {
    name: String,
    config: ProfileConfig,
    token_saved: bool,
}

async fn collect_profile_input(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
) -> Result<CollectedProfile, AppError> {
    collect_profile_input_inner(name, provider, model, base_url, secrets, config, false).await
}

async fn collect_profile_input_with_optional_setup_token(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
) -> Result<CollectedProfile, AppError> {
    collect_profile_input_inner(name, provider, model, base_url, secrets, config, true).await
}

async fn collect_profile_input_inner(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    prompt_setup_token: bool,
) -> Result<CollectedProfile, AppError> {
    let provider = match provider {
        Some(provider) => provider,
        None => prompt_provider()?,
    };
    let name = match name {
        Some(name) => name,
        None => prompt_with_default(
            "Profile name. This is just a local nickname for this provider",
            &provider.to_string(),
        )?,
    };
    let base_url = match base_url {
        Some(base_url) => base_url,
        None => prompt_with_default(
            "Base URL. Press Enter unless you use a custom compatible endpoint",
            provider.default_base_url(),
        )?,
    };
    let token_profile = ProfileConfig {
        provider,
        model: provider.default_model().to_string(),
        base_url: base_url.clone(),
        token_ref: crate::config::token_ref(&provider),
    };
    let setup_token = if prompt_setup_token {
        println!("The token will be stored in the OS keychain, not in config.");
        prompt_optional_secret("API token (paste it, or press Enter to skip)")?
    } else {
        None
    };
    if let Some(token) = setup_token.as_deref() {
        set_profile_token(secrets, &token_profile, token)?;
    }
    let token_saved = setup_token.is_some();
    let model_token = match setup_token {
        Some(token) => Some(token),
        None => get_config_profile_token(secrets, config, &name, &token_profile)?,
    };
    let model = match model {
        Some(model) => model,
        None => prompt_model(provider, &base_url, model_token.as_deref()).await?,
    };

    Ok(CollectedProfile {
        name,
        config: ProfileConfig {
            provider,
            model,
            base_url,
            token_ref: String::new(),
        },
        token_saved,
    })
}

async fn prompt_model(
    provider: ProviderKind,
    base_url: &str,
    token: Option<&str>,
) -> Result<String, AppError> {
    let spec = provider.spec();
    println!();
    println!("Choose model for {}.", spec.display_name);
    let models = match token {
        Some(token) => fetch_provider_models(provider, base_url, token).await?,
        None => Vec::new(),
    };
    if models.is_empty() {
        if token.is_none() {
            println!("No token is available yet, so the live model list cannot be loaded.");
        } else {
            println!("The live model list is empty; type a model id manually if needed.");
        }
        println!(
            "Press Enter for the provider default, or type a model id. Default: {}",
            spec.default_model
        );
    } else {
        println!("Loaded {} models from the provider.", models.len());
        println!(
            "Press Enter for the provider default, choose a number, or type a custom model id."
        );
        for (index, model) in models.iter().enumerate() {
            let recommended = if model == spec.default_model {
                " default"
            } else {
                ""
            };
            println!("  {}. {}{}", index + 1, model, recommended);
        }
        println!("  custom. Type another model id manually");
    }

    loop {
        let raw = prompt(&format!(
            "Model number or custom id [{}]",
            spec.default_model
        ))?;
        if raw.trim().is_empty() {
            return Ok(spec.default_model.to_string());
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(model) = models.get(index.saturating_sub(1))
        {
            return Ok(model.to_string());
        }
        if raw.eq_ignore_ascii_case("custom") {
            return prompt_required("Custom model id");
        }
        if !raw.trim().is_empty() {
            return Ok(raw);
        }
        println!("Choose a model number, type custom, press Enter, or type a model id.");
    }
}

async fn fetch_provider_models(
    provider: ProviderKind,
    base_url: &str,
    token: &str,
) -> Result<Vec<String>, AppError> {
    validate_base_url(&provider.to_string(), base_url)?;
    let client = ReqwestProviderClient::new()?;
    let profile = ProfileConfig {
        provider,
        model: provider.default_model().to_string(),
        base_url: base_url.to_string(),
        token_ref: String::new(),
    };
    match client.list_models(&profile, token).await {
        Ok(models) => Ok(models),
        Err(error) => {
            eprintln!("Could not load live model list: {error}");
            Ok(Vec::new())
        }
    }
}

fn prompt_provider() -> Result<ProviderKind, AppError> {
    println!("Choose provider.");
    println!("If unsure, choose 2 for OpenRouter or press Enter for the recommended default.");
    for (index, provider) in ProviderKind::all().iter().enumerate() {
        let spec = provider.spec();
        let recommended = if *provider == ProviderKind::OpenRouter {
            " recommended"
        } else {
            ""
        };
        println!(
            "  {}. {} ({}) - default model: {}{}",
            index + 1,
            spec.display_name,
            provider,
            spec.default_model,
            recommended
        );
    }

    loop {
        let raw = prompt("Provider number or name [2]")?;
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

fn prompt_required(label: &str) -> Result<String, AppError> {
    loop {
        let value = prompt(label)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        println!("Please enter a value.");
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String, AppError> {
    let raw = prompt(&format!("{label} [{default}]"))?;
    if raw.trim().is_empty() {
        println!("Using default: {default}");
        Ok(default.to_string())
    } else {
        Ok(raw)
    }
}

fn prompt(label: &str) -> Result<String, AppError> {
    print!("{label}: ");
    io::stdout()
        .flush()
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    Ok(read_stdin_line()?.unwrap_or_default().trim().to_string())
}

fn read_stdin_line() -> Result<Option<String>, AppError> {
    let mut bytes = Vec::new();
    let read = io::stdin()
        .lock()
        .read_until(b'\n', &mut bytes)
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    while bytes
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        bytes.pop();
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
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
    let token = get_config_profile_token(secrets, &config, &name, profile)?.ok_or_else(|| {
        AppError::MissingToken {
            profile: name.to_string(),
        }
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
        &config,
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
        &config,
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
        &config,
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
    let token_present = get_config_profile_token(secrets, &config, &name, profile)?.is_some();
    println!(
        "Token: {}",
        if token_present { "present" } else { "missing" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_use_accepts_unquoted_names_with_spaces() {
        let cli = Cli::try_parse_from(["ai-playground", "profile", "use", "ВуDeepSeek", "pro"])
            .expect("parse profile use");

        let Command::Profile { command } = cli.command else {
            panic!("expected profile command");
        };
        let ProfileCommand::Use(args) = command else {
            panic!("expected profile use command");
        };

        assert_eq!(profile_name_from_parts(&args.name), "ВуDeepSeek pro");
    }

    #[test]
    fn profile_use_can_open_interactive_menu_without_name() {
        let cli =
            Cli::try_parse_from(["ai-playground", "profile", "use"]).expect("parse profile use");

        let Command::Profile { command } = cli.command else {
            panic!("expected profile command");
        };
        let ProfileCommand::Use(args) = command else {
            panic!("expected profile use command");
        };

        assert!(args.name.is_empty());
    }

    #[test]
    fn top_level_use_accepts_profile_name() {
        let cli =
            Cli::try_parse_from(["ai-playground", "use", "ВуDeepSeek", "pro"]).expect("parse use");

        let Command::Use(args) = cli.command else {
            panic!("expected use command");
        };

        assert_eq!(profile_name_from_parts(&args.name), "ВуDeepSeek pro");
    }

    #[test]
    fn profile_remove_accepts_unquoted_names_with_spaces() {
        let cli = Cli::try_parse_from(["ai-playground", "profile", "remove", "ВуDeepSeek", "pro"])
            .expect("parse profile remove");

        let Command::Profile { command } = cli.command else {
            panic!("expected profile command");
        };
        let ProfileCommand::Remove { name } = command else {
            panic!("expected profile remove command");
        };

        assert_eq!(profile_name_from_parts(&name), "ВуDeepSeek pro");
    }
}
