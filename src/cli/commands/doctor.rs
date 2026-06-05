use crate::{
    config::AppConfig,
    errors::AppError,
    providers::validate_base_url,
    secrets::{SecretStore, get_config_profile_token},
};
use crate::cli::args::ProfileArg;

pub fn run_doctor(args: &ProfileArg, secrets: &dyn SecretStore) -> Result<(), AppError> {
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
