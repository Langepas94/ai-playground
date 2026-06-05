use crate::{
    config::AppConfig,
    errors::AppError,
    providers::validate_base_url,
    secrets::{SecretStore, delete_legacy_profile_token, get_config_profile_token},
};
use crate::cli::args::{ProfileCommand, ProfileUseArgs};
use crate::cli::{collect_profile_input, profile_name_from_parts, select_profile_name};

pub async fn run_profile_add(
    name: Option<String>,
    provider: Option<crate::providers::ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &mut AppConfig,
) -> Result<(), AppError> {
    let profile = collect_profile_input(name, provider, model, base_url, secrets, config).await?;
    validate_base_url(&profile.name, &profile.config.base_url)?;
    config.add_profile(profile.name.clone(), profile.config);
    config.save()?;
    println!("Profile '{}' added.", profile.name);
    Ok(())
}

pub fn run_profile_list(secrets: &dyn SecretStore, config: &AppConfig) -> Result<(), AppError> {
    for (name, profile) in &config.profiles {
        let active = if config.active_profile.as_deref() == Some(name) {
            "*"
        } else {
            " "
        };
        let has_token = get_config_profile_token(secrets, config, name, profile)?.is_some();
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
    Ok(())
}

pub fn run_profile_use(args: &ProfileUseArgs, config: &mut AppConfig) -> Result<(), AppError> {
    let name = select_profile_name(config, &args.name)?;
    config.use_profile(&name)?;
    config.save()?;
    println!("Active profile: {name}");
    Ok(())
}

pub fn run_profile_remove(
    name_parts: &[String],
    secrets: &dyn SecretStore,
    config: &mut AppConfig,
) -> Result<(), AppError> {
    let name = profile_name_from_parts(name_parts);
    let removed = config.remove_profile(&name)?;
    delete_legacy_profile_token(secrets, &name, &removed)?;
    config.save()?;
    println!("Profile '{name}' removed.");
    Ok(())
}
