use crate::cli::args::{CompareArgs, CompareGoalArgs};
use crate::cli::request_model_runtime_info;
use crate::{
    chat,
    config::AppConfig,
    errors::AppError,
    providers::{ReqwestProviderClient, ResponseControl},
    secrets::SecretStore,
};

pub async fn run_compare(args: &CompareArgs, secrets: &dyn SecretStore) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    let control = ResponseControl::from(&args.control);
    let client = ReqwestProviderClient::new()?;
    let runtime_info =
        request_model_runtime_info(&args.pricing, &client, secrets, &config, &name, profile)
            .await?;
    let billing = args.billing.billing_lookup();
    eprintln!("Waiting for unrestricted and controlled provider responses...");
    let (unrestricted, controlled) = chat::compare_response_control(
        chat::ChatRuntime {
            client: &client,
            secrets,
            config: &config,
        },
        chat::SelectedProfile {
            name: &name,
            config: profile,
        },
        args.prompt.clone(),
        control,
        chat::RequestOptions {
            pricing: runtime_info.pricing,
            billing,
            context_limit: runtime_info.context_limit,
        },
    )
    .await?;
    println!("## Without constraints\n{}\n", unrestricted.text);
    eprintln!(
        "Without constraints metrics:\n{}",
        chat::format_request_metrics(&unrestricted.metrics)
    );
    println!("## With constraints\n{}", controlled.text);
    eprintln!(
        "With constraints metrics:\n{}",
        chat::format_request_metrics(&controlled.metrics)
    );
    Ok(())
}

pub async fn run_compare_goal(
    args: &CompareGoalArgs,
    secrets: &dyn SecretStore,
) -> Result<(), AppError> {
    let config = AppConfig::load()?;
    let (name, profile) = config.selected_profile(args.profile.as_deref())?;
    if args.required_field.is_empty() {
        return Err(AppError::InvalidInput(
            "compare-goal requires at least one --required-field".to_string(),
        ));
    }
    let client = ReqwestProviderClient::new()?;
    let runtime_info =
        request_model_runtime_info(&args.pricing, &client, secrets, &config, &name, profile)
            .await?;
    let billing = args.billing.billing_lookup();
    eprintln!("Waiting for state, instruction, and combined goal-stop responses...");
    let comparison = chat::compare_goal_stop(
        chat::ChatRuntime {
            client: &client,
            secrets,
            config: &config,
        },
        chat::SelectedProfile {
            name: &name,
            config: profile,
        },
        args.prompt.clone(),
        args.required_field.clone(),
        chat::RequestOptions {
            pricing: runtime_info.pricing,
            billing,
            context_limit: runtime_info.context_limit,
        },
    )
    .await?;
    print_goal_run("State-based stop", &comparison.state);
    print_goal_run("Instruction-based stop", &comparison.instruction);
    print_goal_run("Combined stop", &comparison.combined);
    Ok(())
}

fn print_goal_run(title: &str, run: &chat::GoalRun) {
    println!(
        "## {title}\nmode: {}\nstopped: {}\nstate: {}\n{}\n\nmetrics:\n{}",
        run.mode,
        run.stopped,
        run.state_summary,
        run.response,
        chat::format_request_metrics(&run.metrics)
    );
}
