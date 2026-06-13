use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use crate::{
    chat,
    errors::AppError,
    providers::{
        AnswerFormat, BillingLookup, BillingProvider, ModelPricing, ProviderKind, ResponseControl,
        ResponseFormat,
    },
};

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Interactive first-run setup")]
    Setup(SetupArgs),
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    Ask(AskArgs),
    Chat(ChatArgs),
    #[command(about = "Select active profile from a menu or by name")]
    Use(ProfileUseArgs),
    #[command(about = "Start the local web UI")]
    Web(WebArgs),
    #[command(about = "Run the same prompt once without controls and once with response controls")]
    Compare(CompareArgs),
    #[command(about = "Compare state-based, instruction-based, and combined dialogue stopping")]
    CompareGoal(CompareGoalArgs),
    #[command(about = "Show how agent tokens, cost, and context overflow grow across dialogue")]
    TokenDemo(TokenDemoArgs),
    Dist {
        #[command(subcommand)]
        command: DistCommand,
    },
    Pricing {
        #[command(subcommand)]
        command: PricingCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Doctor(ProfileArg),
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Add(ProfileAddArgs),
    List,
    Use(ProfileUseArgs),
    Remove {
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
}

#[derive(Debug, Args)]
pub struct ProfileAddArgs {
    pub name: Option<String>,
    #[arg(long)]
    pub provider: Option<ProviderKind>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub base_url: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProfileUseArgs {
    #[arg(num_args = 0..)]
    pub name: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub provider: Option<ProviderKind>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub base_url: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    Set(ProfileArg),
    Delete(ProfileArg),
}

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    List(ProfileArg),
}

#[derive(Debug, Subcommand)]
pub enum PricingCommand {
    #[command(about = "Download the latest LiteLLM model price catalog")]
    Sync,
    #[command(about = "Show local model price catalog status")]
    Status,
}

#[derive(Debug, Subcommand)]
pub enum DistCommand {
    #[command(about = "Download and install the latest dev build")]
    Install(DistInstallArgs),
    #[command(about = "Update an installed dev build")]
    Update(DistInstallArgs),
    #[command(about = "Show download and install paths")]
    Status(DistInstallArgs),
}

#[derive(Debug, Args, Clone)]
pub struct DistInstallArgs {
    #[arg(long, help = "Override release URL for a specific binary asset")]
    pub url: Option<String>,
    #[arg(long, help = "Override release channel, such as dev or latest")]
    pub channel: Option<String>,
    #[arg(long, help = "Override install directory")]
    pub install_dir: Option<PathBuf>,
    #[arg(long, help = "Overwrite an existing installed binary")]
    pub overwrite: bool,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    pub prompt: String,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Path to a text file whose content is appended to the prompt; can be provided multiple times"
    )]
    pub file: Vec<PathBuf>,
    #[command(flatten)]
    pub control: ResponseControlArgs,
    #[command(flatten)]
    pub pricing: PricingArgs,
    #[command(flatten)]
    pub billing: BillingArgs,
}

#[derive(Debug, Args)]
pub struct CompareArgs {
    pub prompt: String,
    #[arg(long)]
    pub profile: Option<String>,
    #[command(flatten)]
    pub control: ResponseControlArgs,
    #[command(flatten)]
    pub pricing: PricingArgs,
    #[command(flatten)]
    pub billing: BillingArgs,
}

#[derive(Debug, Args)]
pub struct CompareGoalArgs {
    pub prompt: String,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Required entity field; provide at least one"
    )]
    pub required_field: Vec<String>,
    #[command(flatten)]
    pub pricing: PricingArgs,
    #[command(flatten)]
    pub billing: BillingArgs,
}

#[derive(Debug, Args)]
pub struct TokenDemoArgs {
    #[arg(
        long,
        default_value_t = 4096,
        help = "Model context window used by the demo"
    )]
    pub context_limit: u32,
    #[arg(
        long,
        default_value_t = 3,
        help = "Turns in the short dialogue scenario"
    )]
    pub short_turns: usize,
    #[arg(
        long,
        default_value_t = 7,
        help = "Turns in the long dialogue scenario"
    )]
    pub long_turns: usize,
    #[arg(
        long,
        default_value_t = 80,
        help = "Max turns in the overflow scenario"
    )]
    pub overflow_turns: usize,
    #[arg(long, default_value_t = 60, help = "Estimated user tokens per turn")]
    pub user_tokens_per_turn: u32,
    #[arg(
        long,
        default_value_t = 180,
        help = "Estimated model response tokens per turn"
    )]
    pub response_tokens_per_turn: u32,
    #[arg(
        long,
        default_value_t = 1.25,
        help = "Demo input token price per 1M tokens"
    )]
    pub input_price_per_million: f64,
    #[arg(
        long,
        default_value_t = 10.0,
        help = "Demo output token price per 1M tokens"
    )]
    pub output_price_per_million: f64,
    #[arg(long, default_value = "USD", help = "Demo pricing currency label")]
    pub price_currency: String,
}

#[derive(Debug, Args)]
pub struct ChatArgs {
    #[arg(long)]
    pub profile: Option<String>,
    #[command(flatten)]
    pub control: ResponseControlArgs,
    #[command(flatten)]
    pub pricing: PricingArgs,
    #[command(flatten)]
    pub billing: BillingArgs,
    #[command(flatten)]
    pub goal: ConversationGoalArgs,
    #[command(flatten)]
    pub memory: MemoryArgs,
}

#[derive(Debug, Args)]
pub struct WebArgs {
    #[arg(long, default_value = "127.0.0.1:8787")]
    pub listen: SocketAddr,
}

#[derive(Debug, Args)]
pub struct ProfileArg {
    #[arg(long)]
    pub profile: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Path,
}

#[derive(Debug, Clone, Args, Default)]
pub struct ResponseControlArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = CliResponseFormat::Text,
        help = "Response format requested from the provider"
    )]
    pub response_format: CliResponseFormat,
    #[arg(
        long,
        value_enum,
        default_value_t = CliAnswerFormat::Natural,
        help = "Human-facing answer shape, such as bullets, steps, or table"
    )]
    pub answer_format: CliAnswerFormat,
    #[arg(
        long,
        help = "Maximum number of output tokens requested from the provider"
    )]
    pub max_tokens: Option<u32>,
    #[arg(
        long,
        help = "Maximum number of completion tokens requested from newer providers"
    )]
    pub max_completion_tokens: Option<u32>,
    #[arg(long, help = "Sampling temperature requested from the provider")]
    pub temperature: Option<f32>,
    #[arg(
        long,
        help = "Nucleus sampling probability requested from the provider"
    )]
    pub top_p: Option<f32>,
    #[arg(long, help = "Top-k sampling requested from providers that support it")]
    pub top_k: Option<u32>,
    #[arg(long, help = "Min-p sampling requested from providers that support it")]
    pub min_p: Option<f32>,
    #[arg(long, help = "Top-a sampling requested from providers that support it")]
    pub top_a: Option<f32>,
    #[arg(long, help = "Presence penalty requested from the provider")]
    pub presence_penalty: Option<f32>,
    #[arg(long, help = "Frequency penalty requested from the provider")]
    pub frequency_penalty: Option<f32>,
    #[arg(
        long,
        help = "Repetition penalty requested from providers that support it"
    )]
    pub repetition_penalty: Option<f32>,
    #[arg(
        long,
        help = "Deterministic sampling seed, when supported by the provider"
    )]
    pub seed: Option<i64>,
    #[arg(
        long,
        help = "Reasoning effort, such as none, minimal, low, medium, high, or xhigh"
    )]
    pub reasoning_effort: Option<String>,
    #[arg(long, help = "Ask provider to include reasoning, when supported")]
    pub include_reasoning: Option<bool>,
    #[arg(long, help = "Verbosity, such as low, medium, or high")]
    pub verbosity: Option<String>,
    #[arg(long, help = "Request token log probabilities, when supported")]
    pub logprobs: Option<bool>,
    #[arg(long, help = "Number of top log probabilities to return")]
    pub top_logprobs: Option<u32>,
    #[arg(long, help = "Number of choices to generate")]
    pub n: Option<u32>,
    #[arg(long, help = "Whether provider may store this completion")]
    pub store: Option<bool>,
    #[arg(long, help = "Whether tools may be called in parallel")]
    pub parallel_tool_calls: Option<bool>,
    #[arg(long, help = "End-user identifier passed to the provider")]
    pub user: Option<String>,
    #[arg(long, help = "Service tier, such as auto, default, flex, or priority")]
    pub service_tier: Option<String>,
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Stop sequence; can be provided multiple times"
    )]
    pub stop: Vec<String>,
    #[arg(long, help = "Exact text the answer should start with")]
    pub answer_prefix: Option<String>,
    #[arg(long, help = "Exact text the answer should end with")]
    pub answer_suffix: Option<String>,
    #[arg(long, help = "Name or label the answer should address the user with")]
    pub address_as: Option<String>,
    #[arg(long, help = "Ask the model to quote the user's question first")]
    pub quote_question: bool,
    #[arg(
        long,
        help = "System instruction that explicitly describes the response format"
    )]
    pub format_instruction: Option<String>,
    #[arg(
        long,
        help = "System instruction that describes when the answer should finish"
    )]
    pub completion_instruction: Option<String>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct PricingArgs {
    #[arg(long, help = "Exact input token price per 1M tokens for this request")]
    pub input_price_per_million: Option<f64>,
    #[arg(long, help = "Exact output token price per 1M tokens for this request")]
    pub output_price_per_million: Option<f64>,
    #[arg(
        long,
        help = "Exact cached input token price per 1M tokens, when usage reports cache hits"
    )]
    pub cache_hit_input_price_per_million: Option<f64>,
    #[arg(
        long,
        help = "Exact cache-miss input token price per 1M tokens, when usage reports cache misses"
    )]
    pub cache_miss_input_price_per_million: Option<f64>,
    #[arg(
        long,
        default_value = "USD",
        help = "Currency label for configured pricing"
    )]
    pub price_currency: String,
}

impl PricingArgs {
    pub fn model_pricing(&self) -> Result<Option<ModelPricing>, AppError> {
        match (self.input_price_per_million, self.output_price_per_million) {
            (None, None) => Ok(None),
            (Some(input_per_million), Some(output_per_million)) => Ok(Some(ModelPricing {
                currency: self.price_currency.clone(),
                input_per_million: Some(input_per_million),
                output_per_million,
                cache_hit_input_per_million: self.cache_hit_input_price_per_million,
                cache_miss_input_per_million: self.cache_miss_input_price_per_million,
            })),
            _ => Err(AppError::InvalidInput(
                "Set both --input-price-per-million and --output-price-per-million to calculate request cost.".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Args, Default)]
pub struct BillingArgs {
    #[arg(
        long,
        help = "OpenAI Admin API key used to fetch /organization/costs after the request"
    )]
    pub openai_admin_token: Option<String>,
    #[arg(
        long,
        default_value_t = 20,
        help = "Seconds to poll OpenAI /organization/costs for billing data"
    )]
    pub openai_cost_poll_seconds: u64,
}

impl BillingArgs {
    pub fn billing_lookup(&self) -> Option<BillingLookup> {
        self.openai_admin_token
            .as_ref()
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .map(|token| BillingLookup {
                provider: BillingProvider::OpenAiCosts,
                admin_token: token.to_string(),
                poll_seconds: self.openai_cost_poll_seconds,
            })
    }
}

#[derive(Debug, Clone, Args)]
pub struct ConversationGoalArgs {
    #[arg(
        long,
        action = clap::ArgAction::Append,
        help = "Required field for stateful dialogue completion; can be provided multiple times"
    )]
    pub required_field: Vec<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = CliConversationStopMode::Manual,
        help = "How chat decides that the dialogue goal is complete"
    )]
    pub goal_stop_mode: CliConversationStopMode,
}

#[derive(Debug, Clone, Args)]
pub struct MemoryArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = CliMemoryStrategy::Summary,
        help = "How the local agent sends chat history to the provider"
    )]
    pub memory_strategy: CliMemoryStrategy,
    #[arg(
        long,
        default_value_t = chat::memory::DEFAULT_RECENT_MESSAGES,
        help = "How many latest non-system messages are kept in the context window"
    )]
    pub memory_recent_messages: usize,
    #[arg(
        long,
        default_value_t = chat::memory::DEFAULT_SUMMARIZE_AFTER_MESSAGES,
        help = "Summary strategy starts compacting after this many stored messages"
    )]
    pub memory_summarize_after_messages: usize,
    #[arg(
        long,
        default_value_t = chat::memory::DEFAULT_SUMMARY_CHUNK_MESSAGES,
        help = "Minimum unsummarized message chunk size before summary compaction runs"
    )]
    pub memory_summary_chunk_messages: usize,
    #[arg(
        long,
        default_value_t = chat::memory::DEFAULT_SUMMARIZE_AT_CONTEXT_PERCENT,
        help = "Summary strategy precompacts when estimated input reaches this context percent"
    )]
    pub memory_summarize_at_context_percent: u8,
    #[arg(
        long,
        default_value = chat::memory::DEFAULT_SUMMARY_PROMPT,
        help = "System prompt used for Summary strategy compaction requests"
    )]
    pub memory_summary_prompt: String,
    #[arg(
        long,
        default_value = chat::memory::DEFAULT_FACTS_PROMPT,
        help = "System prompt that introduces Sticky Facts in provider requests"
    )]
    pub memory_facts_prompt: String,
    #[arg(
        long,
        default_value = "default",
        help = "Active internal branch for scoped-branches strategy"
    )]
    pub memory_active_branch: String,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum CliResponseFormat {
    #[default]
    Text,
    JsonObject,
    Toon,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum CliAnswerFormat {
    #[default]
    Natural,
    Bullets,
    Numbered,
    Short,
    Steps,
    Table,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum CliConversationStopMode {
    #[default]
    Manual,
    State,
    Instruction,
    Combined,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum CliMemoryStrategy {
    #[default]
    Summary,
    SlidingWindow,
    StickyFacts,
    Branching,
    ScopedBranches,
}

impl From<&ResponseControlArgs> for ResponseControl {
    fn from(args: &ResponseControlArgs) -> Self {
        Self {
            format: match args.response_format {
                CliResponseFormat::Text => ResponseFormat::Text,
                CliResponseFormat::JsonObject => ResponseFormat::JsonObject,
                CliResponseFormat::Toon => ResponseFormat::Toon,
            },
            answer_format: match args.answer_format {
                CliAnswerFormat::Natural => AnswerFormat::Natural,
                CliAnswerFormat::Bullets => AnswerFormat::Bullets,
                CliAnswerFormat::Numbered => AnswerFormat::Numbered,
                CliAnswerFormat::Short => AnswerFormat::Short,
                CliAnswerFormat::Steps => AnswerFormat::Steps,
                CliAnswerFormat::Table => AnswerFormat::Table,
            },
            max_tokens: args.max_tokens,
            max_completion_tokens: args.max_completion_tokens,
            temperature: args.temperature,
            top_p: args.top_p,
            top_k: args.top_k,
            min_p: args.min_p,
            top_a: args.top_a,
            presence_penalty: args.presence_penalty,
            frequency_penalty: args.frequency_penalty,
            repetition_penalty: args.repetition_penalty,
            seed: args.seed,
            reasoning_effort: args.reasoning_effort.clone(),
            include_reasoning: args.include_reasoning,
            verbosity: args.verbosity.clone(),
            logprobs: args.logprobs,
            top_logprobs: args.top_logprobs,
            n: args.n,
            store: args.store,
            parallel_tool_calls: args.parallel_tool_calls,
            user: args.user.clone(),
            service_tier: args.service_tier.clone(),
            extra_params: serde_json::Map::new(),
            stop: args.stop.clone(),
            answer_prefix: args.answer_prefix.clone(),
            answer_suffix: args.answer_suffix.clone(),
            address_as: args.address_as.clone(),
            quote_question: args.quote_question,
            format_instruction: args.format_instruction.clone(),
            completion_instruction: args.completion_instruction.clone(),
        }
    }
}

impl From<&ConversationGoalArgs> for chat::ConversationGoal {
    fn from(args: &ConversationGoalArgs) -> Self {
        Self {
            required_fields: args.required_field.clone(),
            mode: match args.goal_stop_mode {
                CliConversationStopMode::Manual => chat::ConversationStopMode::Manual,
                CliConversationStopMode::State => chat::ConversationStopMode::State,
                CliConversationStopMode::Instruction => chat::ConversationStopMode::Instruction,
                CliConversationStopMode::Combined => chat::ConversationStopMode::Combined,
            },
        }
    }
}

impl From<&MemoryArgs> for chat::MemoryConfig {
    fn from(args: &MemoryArgs) -> Self {
        Self {
            strategy: match args.memory_strategy {
                CliMemoryStrategy::Summary => chat::memory::MemoryStrategy::Summary,
                CliMemoryStrategy::SlidingWindow => chat::memory::MemoryStrategy::SlidingWindow,
                CliMemoryStrategy::StickyFacts => chat::memory::MemoryStrategy::StickyFacts,
                CliMemoryStrategy::Branching => chat::memory::MemoryStrategy::Branching,
                CliMemoryStrategy::ScopedBranches => chat::memory::MemoryStrategy::ScopedBranches,
            },
            recent_messages: args.memory_recent_messages,
            summarize_after_messages: args.memory_summarize_after_messages,
            summary_chunk_messages: args.memory_summary_chunk_messages,
            summarize_at_context_percent: args.memory_summarize_at_context_percent,
            summary_prompt: args.memory_summary_prompt.clone(),
            facts_prompt: args.memory_facts_prompt.clone(),
            active_branch: args.memory_active_branch.clone(),
        }
    }
}

pub fn parse_answer_format(value: &str) -> Option<AnswerFormat> {
    match value {
        "natural" => Some(AnswerFormat::Natural),
        "bullets" => Some(AnswerFormat::Bullets),
        "numbered" => Some(AnswerFormat::Numbered),
        "short" => Some(AnswerFormat::Short),
        "steps" => Some(AnswerFormat::Steps),
        "table" => Some(AnswerFormat::Table),
        _ => None,
    }
}

pub fn parse_stop_mode(value: &str) -> Option<chat::ConversationStopMode> {
    match value {
        "manual" => Some(chat::ConversationStopMode::Manual),
        "state" => Some(chat::ConversationStopMode::State),
        "instruction" => Some(chat::ConversationStopMode::Instruction),
        "combined" => Some(chat::ConversationStopMode::Combined),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_args_convert_custom_facts_prompt() {
        let args = MemoryArgs {
            memory_strategy: CliMemoryStrategy::StickyFacts,
            memory_recent_messages: 5,
            memory_summarize_after_messages: 18,
            memory_summary_chunk_messages: 10,
            memory_summarize_at_context_percent: 80,
            memory_summary_prompt: "Custom summary prompt".to_string(),
            memory_facts_prompt: "Custom facts prompt".to_string(),
            memory_active_branch: "default".to_string(),
        };

        let config = chat::MemoryConfig::from(&args);

        assert_eq!(config.strategy, chat::memory::MemoryStrategy::StickyFacts);
        assert_eq!(config.recent_messages, 5);
        assert_eq!(config.facts_prompt, "Custom facts prompt");
    }

    #[test]
    fn memory_args_convert_custom_summary_prompt_and_thresholds() {
        let args = MemoryArgs {
            memory_strategy: CliMemoryStrategy::Summary,
            memory_recent_messages: 7,
            memory_summarize_after_messages: 9,
            memory_summary_chunk_messages: 3,
            memory_summarize_at_context_percent: 70,
            memory_summary_prompt: "Summarize only stable decisions.".to_string(),
            memory_facts_prompt: chat::memory::DEFAULT_FACTS_PROMPT.to_string(),
            memory_active_branch: "default".to_string(),
        };

        let config = chat::MemoryConfig::from(&args);

        assert_eq!(config.strategy, chat::memory::MemoryStrategy::Summary);
        assert_eq!(config.recent_messages, 7);
        assert_eq!(config.summarize_after_messages, 9);
        assert_eq!(config.summary_chunk_messages, 3);
        assert_eq!(config.summarize_at_context_percent, 70);
        assert_eq!(config.summary_prompt, "Summarize only stable decisions.");
    }
}
