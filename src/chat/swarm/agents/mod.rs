//! The swarm's agent entities. Each is a `SubAgent` class with its own behavior
//! and its own resolved provider/model.
//!
//! - `responder` — the four task-stage responders + the general responder.
//! - `service` — turn-maintenance agents (memory, summary, topic, profile,
//!   invariant).

mod responder;
mod service;

pub use responder::{GeneralAgent, PipelineWorkerAgent, StageAgent};
pub(crate) use responder::{
    STAGE_DONE_MARKER, lifecycle_safe_control, resolved_stage_rules, stage_rules,
    strip_stage_marker,
};
pub(crate) use service::update_active_topic_file;
pub use service::{
    InvariantAgent, MemoryAgent, ProfileAgent, SummaryAgent, TopicAgent, TransitionAgent,
    TransitionDecision,
};

use tokio::time::Duration;

// Per-agent provider timeouts (mirror the previous inline `ChatAgent` constants).
pub(crate) const MEMORY_COMPACT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MEMORY_FACTS_EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const TOPIC_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const STATEFUL_STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// Default system prompt for the Profile agent (long-term interview fill).
pub(crate) const DEFAULT_PROFILE_FILL_PROMPT: &str = "You extract profile field values from a user message. Only return values the user actually stated. Reply with a JSON object mapping field keys to string values; omit fields not mentioned. No prose.";
// Invariant checking is a deterministic code linter (see `InvariantAgent`), not an
// LLM sub-request — so it has no default system prompt.

/// The built-in system prompt a role uses when its swarm config leaves the
/// prompt empty. Surfaced to the UI so each agent's role is visible even before
/// the user customizes it. Empty string ⇒ no role prompt (general / code-check).
pub(crate) fn default_role_prompt(role: super::config::SubAgentRole) -> &'static str {
    use super::config::SubAgentRole;
    match role {
        SubAgentRole::Planning
        | SubAgentRole::Execution
        | SubAgentRole::Validation
        | SubAgentRole::Done => stage_rules(role).unwrap_or_default(),
        SubAgentRole::Summary => crate::chat::memory::DEFAULT_SUMMARY_PROMPT,
        SubAgentRole::Topic => crate::chat::memory::DEFAULT_TOPIC_CLASSIFIER_PROMPT,
        SubAgentRole::Profile => DEFAULT_PROFILE_FILL_PROMPT,
        SubAgentRole::Memory => {
            "Извлекает факты из сообщения и раскладывает по слоям памяти. Если промт пуст — работает локально, без LLM."
        }
        SubAgentRole::General => {
            "Обычный ответчик для чата без активной задачи: помогает пользователю напрямую."
        }
        SubAgentRole::Invariant | SubAgentRole::Worker | SubAgentRole::Task => "",
    }
}
