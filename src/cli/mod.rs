use std::io::{self, BufRead, Write};

use anyhow::Result;
use clap::Parser;

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{ModelPricing, ProviderClient, ProviderKind, ReqwestProviderClient, validate_base_url},
    secrets::{KeyringSecretStore, SecretStore, get_config_profile_token, set_profile_token},
};

pub mod args;
pub mod commands;

use args::{
    Command, ConfigCommand, ProfileArg, ProfileCommand, ProfileUseArgs, SetupArgs, TokenCommand,
    ModelsCommand, PricingArgs,
};
use commands::{
    run_ask, run_chat, run_compare, run_compare_goal, run_config_path, run_doctor, run_models_list,
    run_profile_add, run_profile_list, run_profile_remove, run_profile_use, run_setup,
    run_token_delete, run_token_set,
};

#[derive(Debug, Parser)]
#[command(name = "ai", version, about = "Rust CLI for LLM chat")]
pub struct Cli {
    #[arg(long, global = true)]
    verbose: bool,
    #[arg(long, global = true)]
    log_conversation: bool,
    #[command(subcommand)]
    command: Command,
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
        Command::Setup(args) => run_setup(args, secrets).await,
        Command::Profile { command } => profile_command(command, secrets).await,
        Command::Token { command } => token_command(command, secrets),
        Command::Models { command } => models_command(command, secrets).await,
        Command::Ask(args) => run_ask(args, secrets).await,
        Command::Chat(args) => run_chat(args, secrets).await,
        Command::Use(args) => use_profile_command(args),
        Command::Web(args) => crate::web::serve(args.listen).await,
        Command::Compare(args) => run_compare(args, secrets).await,
        Command::CompareGoal(args) => run_compare_goal(args, secrets).await,
        Command::Config { command } => match command {
            ConfigCommand::Path => run_config_path(),
        },
        Command::Doctor(args) => run_doctor(args, secrets),
    }
}

async fn profile_command(
    command: &ProfileCommand,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    match command {
        ProfileCommand::Add(args) => {
            run_profile_add(
                args.name.clone(),
                args.provider,
                args.model.clone(),
                args.base_url.clone(),
                secrets,
                &mut config,
            )
            .await
        }
        ProfileCommand::List => run_profile_list(secrets, &config),
        ProfileCommand::Use(args) => run_profile_use(args, &mut config),
        ProfileCommand::Remove { name } => run_profile_remove(name, secrets, &mut config),
    }
}

fn token_command(command: &TokenCommand, secrets: &dyn SecretStore) -> Result<(), AppError> {
    match command {
        TokenCommand::Set(args) => run_token_set(args.profile.as_deref(), secrets),
        TokenCommand::Delete(args) => run_token_delete(args.profile.as_deref(), secrets),
    }
}

async fn models_command(
    command: &ModelsCommand,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let ModelsCommand::List(args) = command;
    run_models_list(args.profile.as_deref(), secrets).await
}

fn use_profile_command(args: &ProfileUseArgs) -> Result<(), AppError> {
    let mut config = AppConfig::load()?;
    let name = select_profile_name(&config, &args.name)?;
    config.use_profile(&name)?;
    config.save()?;
    println!("Active profile: {name}");
    Ok(())
}

// ── Shared helpers used by command modules ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CollectedProfile {
    pub name: String,
    pub config: ProfileConfig,
    pub token_saved: bool,
}

pub async fn collect_profile_input(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
) -> Result<CollectedProfile, AppError> {
    collect_profile_input_inner(name, provider, model, base_url, secrets, config, false).await
}

pub async fn collect_profile_input_with_optional_setup_token(
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

pub async fn prompt_model(
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

pub fn prompt_provider() -> Result<ProviderKind, AppError> {
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

pub fn prompt_with_default(label: &str, default: &str) -> Result<String, AppError> {
    let raw = prompt(&format!("{label} [{default}]"))?;
    if raw.trim().is_empty() {
        println!("Using default: {default}");
        Ok(default.to_string())
    } else {
        Ok(raw)
    }
}

pub fn prompt(label: &str) -> Result<String, AppError> {
    print!("{label}: ");
    io::stdout()
        .flush()
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    Ok(read_stdin_line()?.unwrap_or_default().trim().to_string())
}

pub fn read_stdin_line() -> Result<Option<String>, AppError> {
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

pub fn select_profile_name(config: &AppConfig, parts: &[String]) -> Result<String, AppError> {
    if !parts.is_empty() {
        return Ok(profile_name_from_parts(parts));
    }
    if config.profiles.is_empty() {
        return Err(AppError::InvalidInput(
            "No profiles found. Run `ai setup` or `ai profile add` first.".to_string(),
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

pub fn profile_name_from_parts(parts: &[String]) -> String {
    parts.join(" ")
}

pub async fn request_pricing(
    pricing_args: &PricingArgs,
    client: &ReqwestProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<Option<ModelPricing>, AppError> {
    if let Some(pricing) = pricing_args.model_pricing()? {
        return Ok(Some(pricing));
    }
    let Some(token) = get_config_profile_token(secrets, config, profile_name, profile)? else {
        return Ok(None);
    };
    match client.list_model_info(profile, &token).await {
        Ok(models) => Ok(models
            .into_iter()
            .find(|model| model.id == profile.model)
            .and_then(|model| model.pricing)),
        Err(error) => {
            eprintln!("Could not load model pricing from /models: {error}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use args::{ProfileCommand, ProfileUseArgs};

    #[test]
    fn profile_use_accepts_unquoted_names_with_spaces() {
        let cli = Cli::try_parse_from(["ai", "profile", "use", "ВуDeepSeek", "pro"])
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
        let cli = Cli::try_parse_from(["ai", "profile", "use"]).expect("parse profile use");

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
        let cli = Cli::try_parse_from(["ai", "use", "ВуDeepSeek", "pro"]).expect("parse use");

        let Command::Use(args) = cli.command else {
            panic!("expected use command");
        };

        assert_eq!(profile_name_from_parts(&args.name), "ВуDeepSeek pro");
    }

    #[test]
    fn profile_remove_accepts_unquoted_names_with_spaces() {
        let cli = Cli::try_parse_from(["ai", "profile", "remove", "ВуDeepSeek", "pro"])
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
