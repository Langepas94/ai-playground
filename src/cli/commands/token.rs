use std::io::{self, Write};

use crate::{
    config::AppConfig,
    errors::AppError,
    secrets::{SecretStore, delete_profile_token, set_profile_token},
};
use crate::cli::read_stdin_line;

pub fn run_token_set(
    profile_arg: Option<&str>,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(profile_arg)?;
    eprint!("Token for profile '{name}': ");
    io::stderr()
        .flush()
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    let token = read_stdin_line()?.unwrap_or_default();
    set_profile_token(secrets, profile, token.trim())?;
    println!("Token saved for provider '{}'.", profile.provider);
    Ok(())
}

pub fn run_token_delete(
    profile_arg: Option<&str>,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(profile_arg)?;
    delete_profile_token(secrets, &name, profile)?;
    println!("Token deleted for provider '{}'.", profile.provider);
    Ok(())
}
