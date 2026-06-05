use crate::{
    chat,
    config::AppConfig,
    errors::AppError,
    providers::{ReqwestProviderClient, ResponseControl},
    secrets::SecretStore,
};
use crate::cli::args::ChatArgs;
use crate::cli::request_pricing;

pub async fn run_chat(args: &ChatArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let client = ReqwestProviderClient::new()?;
    let pricing = request_pricing(&args.pricing, &client, secrets, &config, &name, profile).await?;
    let billing = args.billing.billing_lookup();
    chat::interactive_chat(
        &client,
        secrets,
        &config,
        &name,
        profile,
        ResponseControl::from(&args.control),
        pricing,
        billing,
        chat::ConversationGoal::from(&args.goal),
    )
    .await
}
