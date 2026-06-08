use crate::{
    config::AppConfig,
    errors::AppError,
    providers::{ModelInfo, ReqwestProviderClient},
    secrets::{SecretStore, get_config_profile_token},
};

pub async fn run_models_list(
    profile_arg: Option<&str>,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(profile_arg)?;
    let token = get_config_profile_token(secrets, &config, &name, profile)?.ok_or_else(|| {
        AppError::MissingToken {
            profile: name.to_string(),
        }
    })?;
    eprintln!("Waiting for provider model list...");
    let client = ReqwestProviderClient::new()?;
    for model in client.list_model_info(profile, &token).await? {
        println!("{}", format_model_line(&model));
    }
    Ok(())
}

fn format_model_line(model: &ModelInfo) -> String {
    let Some(pricing) = model.pricing.as_ref() else {
        return model.id.clone();
    };
    format!(
        "{}\tinput={:.8}/{}/1M output={:.8}/{}/1M",
        model.id,
        pricing.input_per_million.unwrap_or(0.0),
        pricing.currency,
        pricing.output_per_million,
        pricing.currency
    )
}
