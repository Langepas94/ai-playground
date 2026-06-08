use crate::cli::args::SetupArgs;
use crate::cli::collect_profile_input_with_optional_setup_token;
use crate::{
    config::AppConfig, errors::AppError, providers::validate_base_url, secrets::SecretStore,
};

pub async fn run_setup(args: &SetupArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    println!("ai setup");
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
            "Token skipped. Add it later with `ai token set --profile {}`.",
            profile.name
        );
    }
    println!();
    println!("Next commands:");
    println!("  ai doctor");
    println!("  ai models list");
    println!("  ai ask \"Сколько будет 3 + 2?\"");
    println!("  ai chat");
    Ok(())
}
