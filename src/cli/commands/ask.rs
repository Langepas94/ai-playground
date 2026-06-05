use crate::{
    chat,
    config::AppConfig,
    errors::AppError,
    providers::{ReqwestProviderClient, ResponseControl},
    secrets::SecretStore,
};
use crate::cli::args::AskArgs;
use crate::cli::request_pricing;

pub async fn run_ask(args: &AskArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    eprintln!("Waiting for provider response...");
    let client = ReqwestProviderClient::new()?;
    let pricing = request_pricing(&args.pricing, &client, secrets, &config, &name, profile).await?;
    let billing = args.billing.billing_lookup();
    let response = chat::ask_once(
        &client,
        secrets,
        &config,
        &name,
        profile,
        args.prompt.clone(),
        ResponseControl::from(&args.control),
        pricing,
        billing,
    )
    .await?;
    println!("{}", response.text);
    eprintln!("{}", chat::format_request_metrics(&response.metrics));
    Ok(())
}
