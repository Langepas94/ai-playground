use crate::cli::args::AskArgs;
use crate::cli::request_model_runtime_info;
use crate::{
    chat,
    config::AppConfig,
    errors::AppError,
    providers::{ReqwestProviderClient, ResponseControl},
    secrets::SecretStore,
};

pub async fn run_ask(args: &AskArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    eprintln!("Waiting for provider response...");
    let client = ReqwestProviderClient::new()?;
    let runtime_info =
        request_model_runtime_info(&args.pricing, &client, secrets, &config, &name, profile)
            .await?;
    let billing = args.billing.billing_lookup();
    let prompt = build_prompt_with_files(&args.prompt, &args.file)?;
    let response = chat::ask_once(
        chat::ChatRuntime {
            client: &client,
            secrets,
            config: &config,
        },
        chat::SelectedProfile {
            name: &name,
            config: profile,
        },
        prompt,
        ResponseControl::from(&args.control),
        chat::RequestOptions {
            pricing: runtime_info.pricing,
            billing,
            context_limit: runtime_info.context_limit,
        },
    )
    .await?;
    println!("{}", response.text);
    eprintln!("{}", chat::format_request_metrics(&response.metrics));
    Ok(())
}

/// Reads each file and appends its content to the prompt, separated by a labelled block.
pub fn build_prompt_with_files(
    prompt: &str,
    files: &[std::path::PathBuf],
) -> Result<String, AppError> {
    if files.is_empty() {
        return Ok(prompt.to_string());
    }
    let mut parts = vec![prompt.to_string()];
    for path in files {
        let content = std::fs::read_to_string(path).map_err(|error| {
            AppError::InvalidInput(format!("Cannot read file '{}': {error}", path.display()))
        })?;
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        parts.push(format!("--- {label} ---\n{content}"));
    }
    Ok(parts.join("\n\n"))
}
