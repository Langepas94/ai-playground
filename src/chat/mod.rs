pub mod agent;
pub mod goal;
pub mod history;
pub mod memory;
pub mod session;
pub mod store;
pub mod token_accounting;

pub use agent::{
    AgentDescriptor, ChatAgent, LOCAL_SESSION_AGENT_ID, available_agents, selected_agent,
};
pub use goal::{
    ConversationGoal, ConversationStopMode, GoalComparison, GoalRun, GoalState, run_goal_once,
};
pub use history::save_history;
pub use memory::{AgentMemory, MemoryConfig};
pub use session::{describe_control, describe_goal, interactive_chat, read_terminal_line};
pub use store::{
    ConversationSession, LocalSessionStore, add_request_metrics, session_key, web_session_key,
};
pub use token_accounting::{
    ContextStatus, TokenCostEstimate, TokenEstimate, TokenGrowthReport, TokenGrowthRow,
    estimate_exchange, estimate_messages_tokens, simulate_growth,
};

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ModelPricing, ProviderClient, RequestMetrics,
        ResponseControl, Role,
    },
    secrets::{SecretStore, get_config_profile_token},
};

#[derive(Clone, Copy)]
pub struct ChatRuntime<'a> {
    pub client: &'a dyn ProviderClient,
    pub secrets: &'a dyn SecretStore,
    pub config: &'a AppConfig,
}

#[derive(Clone, Copy)]
pub struct SelectedProfile<'a> {
    pub name: &'a str,
    pub config: &'a ProfileConfig,
}

#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub pricing: Option<ModelPricing>,
    pub billing: Option<BillingLookup>,
    pub context_limit: Option<u32>,
}

pub async fn ask_once(
    runtime: ChatRuntime<'_>,
    profile: SelectedProfile<'_>,
    prompt: String,
    control: ResponseControl,
    options: RequestOptions,
) -> Result<crate::providers::ChatResponse, AppError> {
    let token = resolve_profile_token(runtime, profile)?;
    let mut agent = ChatAgent::new(
        profile.config.clone(),
        token,
        Vec::new(),
        AgentMemory::default(),
        control.clone(),
        options.pricing,
        options.billing,
    );
    agent.set_context_limit(options.context_limit);
    agent.respond(runtime.client, prompt).await
}

pub async fn compare_response_control(
    runtime: ChatRuntime<'_>,
    profile: SelectedProfile<'_>,
    prompt: String,
    controlled: ResponseControl,
    options: RequestOptions,
) -> Result<
    (
        crate::providers::ChatResponse,
        crate::providers::ChatResponse,
    ),
    AppError,
> {
    let token = resolve_profile_token(runtime, profile)?;
    let make_request = |control, prompt: String| ChatRequest {
        model: profile.config.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt,
        }],
        control,
        pricing: options.pricing.clone(),
        billing: options.billing.clone(),
    };
    let unrestricted = runtime
        .client
        .chat_completion(
            profile.config,
            &token,
            make_request(ResponseControl::uncontrolled(), prompt.clone()),
        )
        .await?;
    let restricted = runtime
        .client
        .chat_completion(profile.config, &token, make_request(controlled, prompt))
        .await?;
    Ok((unrestricted, restricted))
}

pub async fn compare_goal_stop(
    runtime: ChatRuntime<'_>,
    profile: SelectedProfile<'_>,
    prompt: String,
    required_fields: Vec<String>,
    options: RequestOptions,
) -> Result<GoalComparison, AppError> {
    let token = resolve_profile_token(runtime, profile)?;
    let run = |mode| {
        run_goal_once(
            runtime.client,
            profile.config,
            &token,
            prompt.clone(),
            &required_fields,
            mode,
            options.clone(),
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

pub(crate) fn resolve_profile_token(
    runtime: ChatRuntime<'_>,
    profile: SelectedProfile<'_>,
) -> Result<String, AppError> {
    get_config_profile_token(
        runtime.secrets,
        runtime.config,
        profile.name,
        profile.config,
    )?
    .ok_or_else(|| AppError::MissingToken {
        profile: profile.name.to_string(),
    })
}

pub fn format_request_metrics(metrics: &RequestMetrics) -> String {
    let usage = metrics
        .usage
        .as_ref()
        .map(|u| {
            let mut parts = vec![
                format!("prompt={}", u.input_tokens),
                format!("completion={}", u.output_tokens),
                format!("total={}", u.total_tokens),
            ];
            push_optional_metric(&mut parts, "prompt_cached", u.cache_hit_input_tokens);
            push_optional_metric(&mut parts, "prompt_uncached", u.cache_miss_input_tokens);
            push_optional_metric(&mut parts, "prompt_audio", u.input_audio_tokens);
            push_optional_metric(&mut parts, "visible_output", u.output_visible_tokens);
            push_optional_metric(&mut parts, "reasoning", u.output_reasoning_tokens);
            push_optional_metric(&mut parts, "output_audio", u.output_audio_tokens);
            push_optional_metric(
                &mut parts,
                "accepted_prediction",
                u.accepted_prediction_output_tokens,
            );
            push_optional_metric(
                &mut parts,
                "rejected_prediction",
                u.rejected_prediction_output_tokens,
            );
            format!("tokens: {}", parts.join(" "))
        })
        .unwrap_or_else(|| "tokens: unavailable".to_string());
    let cost = metrics
        .cost
        .as_ref()
        .map(|c| format!("cost: {:.8} {} ({})", c.amount, c.currency, c.source))
        .unwrap_or_else(|| "cost: unavailable".to_string());
    format!("time: {} ms\n{usage}\n{cost}", metrics.elapsed_ms)
}

fn push_optional_metric(parts: &mut Vec<String>, label: &str, value: Option<u32>) {
    if let Some(value) = value.filter(|value| *value > 0) {
        parts.push(format!("{label}={value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::TokenUsage;

    #[test]
    fn request_metrics_hide_zero_optional_token_details() {
        let formatted = format_request_metrics(&RequestMetrics {
            elapsed_ms: 10,
            usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                cache_hit_input_tokens: Some(12),
                input_audio_tokens: Some(0),
                output_reasoning_tokens: Some(0),
                output_audio_tokens: Some(0),
                accepted_prediction_output_tokens: Some(0),
                rejected_prediction_output_tokens: Some(0),
                ..TokenUsage::default()
            }),
            cost: None,
        });

        assert!(formatted.contains("prompt=100"));
        assert!(formatted.contains("completion=50"));
        assert!(formatted.contains("total=150"));
        assert!(formatted.contains("prompt_cached=12"));
        assert!(!formatted.contains("prompt_audio=0"));
        assert!(!formatted.contains("reasoning=0"));
        assert!(!formatted.contains("accepted_prediction=0"));
    }
}
