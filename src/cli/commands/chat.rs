use crate::cli::args::ChatArgs;
use crate::cli::request_model_runtime_info;
use crate::{
    chat,
    config::AppConfig,
    errors::AppError,
    providers::{ReqwestProviderClient, ResponseControl},
    secrets::SecretStore,
};

pub async fn run_chat(args: &ChatArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let client = ReqwestProviderClient::new()?;
    let runtime_info =
        request_model_runtime_info(&args.pricing, &client, secrets, &config, &name, profile)
            .await?;
    let billing = args.billing.billing_lookup();
    chat::interactive_chat(
        chat::ChatRuntime {
            client: &client,
            secrets,
            config: &config,
        },
        chat::SelectedProfile {
            name: &name,
            config: profile,
        },
        ResponseControl::from(&args.control),
        chat::RequestOptions {
            pricing: runtime_info.pricing,
            billing,
            context_limit: runtime_info.context_limit,
        },
        chat::ConversationGoal::from(&args.goal),
        chat::MemoryConfig::from(&args.memory),
    )
    .await
}
