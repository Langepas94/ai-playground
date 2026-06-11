use crate::{errors::AppError, pricing::LiteLlmPriceCatalog};

pub async fn run_pricing_sync() -> Result<(), AppError> {
    let catalog = LiteLlmPriceCatalog::new()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(AppError::from)?;
    let status = catalog.sync(&client).await?;
    println!(
        "Synced {} models from {}\ncache: {}\nfetched_at_unix: {}",
        status.model_count,
        status.source_url,
        status.path.display(),
        status.fetched_at_unix.unwrap_or_default()
    );
    Ok(())
}

pub fn run_pricing_status() -> Result<(), AppError> {
    let catalog = LiteLlmPriceCatalog::new()?;
    let status = catalog.status()?;
    println!(
        "Price catalog: {}\nexists: {}\nmodels: {}\nstale: {}\nfetched_at_unix: {}\nsource: {}",
        status.path.display(),
        status.exists,
        status.model_count,
        status.stale,
        status
            .fetched_at_unix
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        status.source_url
    );
    Ok(())
}
