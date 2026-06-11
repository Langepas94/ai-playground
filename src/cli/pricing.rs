use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    pricing::LiteLlmPriceCatalog,
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
    let catalog = LiteLlmPriceCatalog::new()?;
    if let Err(error) = catalog.sync_if_stale(client.http_client()).await {
        eprintln!("Could not sync LiteLLM price catalog: {error}");
    }
    match catalog.resolve(profile.provider, &profile.model) {
        Ok(Some(resolution)) => {
            eprintln!(
                "Using model pricing from {}: input={:.8} {}/1M output={:.8} {}/1M{}",
                resolution.source,
                resolution.pricing.input_per_million.unwrap_or(0.0),
                resolution.pricing.currency,
                resolution.pricing.output_per_million,
                resolution.pricing.currency,
                if resolution.stale { " (stale)" } else { "" }
            );
            return Ok(Some(resolution.pricing));
        }
        Ok(None) => {}
        Err(error) => eprintln!("Could not read LiteLLM price catalog: {error}"),
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
