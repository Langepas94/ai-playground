use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{ModelPricing, ReqwestProviderClient},
    secrets::{SecretStore, get_config_profile_token},
};

use super::args::PricingArgs;

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
