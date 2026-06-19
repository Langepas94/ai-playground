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
    STAGE_DONE_MARKER, lifecycle_safe_control, stage_rules, strip_stage_marker,
};
pub(crate) use service::update_active_topic_file;
pub use service::{InvariantAgent, MemoryAgent, ProfileAgent, SummaryAgent, TopicAgent};

use tokio::time::Duration;

// Per-agent provider timeouts (mirror the previous inline `ChatAgent` constants).
pub(crate) const MEMORY_COMPACT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MEMORY_FACTS_EXTRACT_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const TOPIC_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const STATEFUL_STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// Default system prompt for the Profile agent (long-term interview fill).
pub(crate) const DEFAULT_PROFILE_FILL_PROMPT: &str = "You extract profile field values from a user message. Only return values the user actually stated. Reply with a JSON object mapping field keys to string values; omit fields not mentioned. No prose.";
/// Default system prompt for the Invariant agent (constraint checker).
pub(crate) const DEFAULT_INVARIANT_CHECK_PROMPT: &str = "You are a strict invariant checker. Return ONLY constraints the assistant response CLEARLY and CONCRETELY breaks — e.g. it recommends or produces something a constraint forbids. Asking questions, clarifying, planning, or simply not mentioning a constraint is NOT a violation. When in doubt, return nothing. Reply with JSON {\"violations\":[\"<exact constraint text>\", ...]}; empty array if the response is fine. No prose.";
