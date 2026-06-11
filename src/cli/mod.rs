use anyhow::Result;
use clap::Parser;

use crate::{
    config::AppConfig,
    errors::AppError,
    secrets::{KeyringSecretStore, SecretStore},
};

pub mod args;
pub mod commands;
mod input;
mod pricing;
mod profile_input;

pub(crate) use input::read_stdin_line;
pub(crate) use pricing::request_pricing;
pub(crate) use profile_input::{
    collect_profile_input, collect_profile_input_with_optional_setup_token,
    profile_name_from_parts, select_profile_name,
};

use args::{
    Command, ConfigCommand, ModelsCommand, PricingCommand, ProfileCommand, ProfileUseArgs,
    TokenCommand,
};
use commands::{
    run_ask, run_chat, run_compare, run_compare_goal, run_config_path, run_doctor, run_models_list,
    run_pricing_status, run_pricing_sync, run_profile_add, run_profile_list, run_profile_remove,
    run_profile_use, run_setup, run_token_delete, run_token_demo, run_token_set,
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
        Command::TokenDemo(args) => {
            run_token_demo(args);
            Ok(())
        }
        Command::Pricing { command } => pricing_command(command).await,
        Command::Config { command } => match command {
            ConfigCommand::Path => run_config_path(),
        },
        Command::Doctor(args) => run_doctor(args, secrets),
    }
}

async fn pricing_command(command: &PricingCommand) -> Result<(), AppError> {
    match command {
        PricingCommand::Sync => run_pricing_sync().await,
        PricingCommand::Status => run_pricing_status(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use args::ProfileCommand;

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
