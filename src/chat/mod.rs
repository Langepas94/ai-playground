pub mod agent;
pub mod goal;
pub mod history;
pub mod session;
pub mod store;

pub use agent::{
    AgentDescriptor, ChatAgent, LOCAL_SESSION_AGENT_ID, available_agents, selected_agent,
};
pub use goal::{
    ConversationGoal, ConversationStopMode, GoalComparison, GoalRun, GoalState, run_goal_once,
};
pub use history::save_history;
pub use session::{describe_control, describe_goal, interactive_chat, read_terminal_line};
pub use store::{ConversationSession, LocalSessionStore, session_key, web_session_key};

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ModelPricing, ProviderClient, RequestMetrics,
        ResponseControl, Role,
    },
    secrets::{SecretStore, get_config_profile_token},
};

pub async fn ask_once(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    control: ResponseControl,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
) -> Result<crate::providers::ChatResponse, AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    ChatAgent::new(
        profile.clone(),
        token,
        Vec::new(),
        control.clone(),
        pricing,
        billing,
    )
    .respond(client, prompt)
    .await
}

pub async fn compare_response_control(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    controlled: ResponseControl,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
) -> Result<
    (
        crate::providers::ChatResponse,
        crate::providers::ChatResponse,
    ),
    AppError,
> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let make_request = |control, prompt: String| ChatRequest {
        model: profile.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt,
        }],
        control,
        pricing: pricing.clone(),
        billing: billing.clone(),
    };
    let unrestricted = client
        .chat_completion(
            profile,
            &token,
            make_request(ResponseControl::uncontrolled(), prompt.clone()),
        )
        .await?;
    let restricted = client
        .chat_completion(profile, &token, make_request(controlled, prompt))
        .await?;
    Ok((unrestricted, restricted))
}

pub async fn compare_goal_stop(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    required_fields: Vec<String>,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
) -> Result<GoalComparison, AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let run = |mode| {
        run_goal_once(
            client,
            profile,
            &token,
            prompt.clone(),
            &required_fields,
            mode,
            pricing.clone(),
            billing.clone(),
        )
    };
    let state = run(ConversationStopMode::State).await?;
    let instruction = run(ConversationStopMode::Instruction).await?;
    let combined = run(ConversationStopMode::Combined).await?;
    Ok(GoalComparison {
        state,
        instruction,
        combined,
    })
}

pub fn format_request_metrics(metrics: &RequestMetrics) -> String {
    let usage = metrics
        .usage
        .as_ref()
        .map(|u| {
            format!(
                "tokens: input={} output={} total={}",
                u.input_tokens, u.output_tokens, u.total_tokens
            )
        })
        .unwrap_or_else(|| "tokens: unavailable".to_string());
    let cost = metrics
        .cost
        .as_ref()
        .map(|c| format!("cost: {:.8} {} ({})", c.amount, c.currency, c.source))
        .unwrap_or_else(|| "cost: unavailable".to_string());
    format!("time: {} ms\n{usage}\n{cost}", metrics.elapsed_ms)
}
