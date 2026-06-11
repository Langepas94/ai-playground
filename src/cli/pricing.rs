use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    pricing::LiteLlmPriceCatalog,
    providers::{ModelPricing, ReqwestProviderClient},
    secrets::{SecretStore, get_config_profile_token},
};

use super::args::PricingArgs;

#[derive(Debug, Clone, Default)]
pub struct ModelRuntimeInfo {
    pub pricing: Option<ModelPricing>,
    pub context_limit: Option<u32>,
}

pub async fn request_model_runtime_info(
    pricing_args: &PricingArgs,
    client: &ReqwestProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
) -> Result<ModelRuntimeInfo, AppError> {
    let manual_pricing = pricing_args.model_pricing()?;
    let mut runtime = ModelRuntimeInfo {
        pricing: manual_pricing.clone(),
        context_limit: None,
    };
    let use_catalog_pricing = manual_pricing.is_none();
    let use_catalog_context = runtime.context_limit.is_none();
    if !use_catalog_pricing && !use_catalog_context {
        return Ok(runtime);
    }

    let catalog = LiteLlmPriceCatalog::new()?;
    if let Err(error) = catalog.sync_if_stale(client.http_client()).await {
        eprintln!("Could not sync LiteLLM price catalog: {error}");
    }
    match catalog.resolve(profile.provider, &profile.model) {
        Ok(Some(resolution)) => {
            if use_catalog_pricing {
                eprintln!(
                    "Using model pricing from {}: input={:.8} {}/1M output={:.8} {}/1M{}",
                    resolution.source,
                    resolution.pricing.input_per_million.unwrap_or(0.0),
                    resolution.pricing.currency,
                    resolution.pricing.output_per_million,
                    resolution.pricing.currency,
                    if resolution.stale { " (stale)" } else { "" }
                );
                runtime.pricing = Some(resolution.pricing.clone());
            }
            if use_catalog_context {
                runtime.context_limit = resolution
                    .context_length
                    .and_then(|limit| u32::try_from(limit).ok());
            }
            if runtime.pricing.is_some() && runtime.context_limit.is_some() {
                return Ok(runtime);
            }
        }
        Ok(None) => {}
        Err(error) => eprintln!("Could not read LiteLLM price catalog: {error}"),
    }
    let Some(token) = get_config_profile_token(secrets, config, profile_name, profile)? else {
        return Ok(runtime);
    };
    match client.list_model_info(profile, &token).await {
        Ok(models) => {
            if let Some(model) = models.into_iter().find(|model| model.id == profile.model) {
                if runtime.pricing.is_none() {
                    runtime.pricing = model.pricing;
                }
                if runtime.context_limit.is_none() {
                    runtime.context_limit = model
                        .context_length
                        .and_then(|limit| u32::try_from(limit).ok());
                }
            }
            Ok(runtime)
        }
        Err(error) => {
            eprintln!("Could not load model pricing from /models: {error}");
            Ok(runtime)
        }
    }
}
