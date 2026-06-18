//! The agent swarm. Every agent is a first-class entity (`trait SubAgent` —
//! "agent = class"), driven by the deterministic [`SwarmOrchestrator`]. The
//! client sees one chat window; under the hood the orchestrator routes a turn
//! across mandatory agents (configurable, never disabled):
//!
//! - 4 task-stage responders (Planning/Execution/Validation/Done) + a General
//!   responder for plain chat.
//! - Service agents: Memory, Summary, Topic, Profile, Invariant.
//! - [`PromptBuilder`](prompt_builder::PromptBuilder) assembles the final prompt
//!   (layers, deduped).
//!
//! Layout:
//! - [`config`] — persisted, token-free [`SwarmConfig`].
//! - [`runtime`] — resolves it into per-role profiles + tokens ([`ResolvedSwarm`]).
//! - [`agent`] — the [`SubAgent`](agent::SubAgent) trait + per-turn [`SwarmTurn`](agent::SwarmTurn).
//! - [`agents`] — the agent entities.
//! - [`orchestrator`] — the deterministic code router.

pub mod agent;
pub mod agents;
pub mod config;
pub mod orchestrator;
pub mod prompt_builder;
pub mod runtime;

#[cfg(test)]
mod orchestrator_tests;
#[cfg(test)]
mod tests;

pub use agent::{SubAgent, SubAgentOutcome, SwarmTurn};
pub use config::{SubAgentConfig, SubAgentRole, SwarmConfig};
pub use orchestrator::SwarmOrchestrator;
pub use prompt_builder::PromptBuilder;
pub use runtime::{ResolvedSubAgent, ResolvedSwarm, resolve_swarm};

use crate::providers::RequestMetrics;

/// What one sub-agent did during a turn. Surfaced in the chat debug payload so
/// the UI swarm panel can show per-agent activity.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SwarmRunRecord {
    pub role: SubAgentRole,
    /// Whether the sub-agent actually fired a request this turn.
    pub ran: bool,
    /// Legacy compatibility mirror. Mandatory sub-agents are always enabled.
    pub enabled: bool,
    /// Model the sub-agent used (resolved).
    pub model: String,
    /// Short human-readable note (e.g. "facts updated", "skipped: not summary").
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RequestMetrics>,
}

impl SwarmRunRecord {
    pub fn new(role: SubAgentRole, model: impl Into<String>) -> Self {
        Self {
            role,
            ran: false,
            enabled: true,
            model: model.into(),
            note: String::new(),
            metrics: None,
        }
    }
}

/// Per-turn report of swarm activity.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SwarmReport {
    pub records: Vec<SwarmRunRecord>,
}

impl SwarmReport {
    pub fn record(&mut self, record: SwarmRunRecord) {
        self.records.push(record);
    }
}
