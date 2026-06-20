//! Swarm configuration: the persisted, token-free description of every
//! sub-agent ("logical chain") that runs alongside the main chat agent.
//!
//! Each [`SubAgentRole`] is a first-class worker with its own provider/model
//! override and system prompt. Empty override strings mean
//! "inherit the main agent". This type is serialized with [`super::super::SavedAgent`]
//! and must never carry a token.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// One agent in the swarm — a first-class entity (`trait SubAgent`), not a
/// toggle. Stage roles are responders driven by the deterministic orchestrator;
/// service roles maintain the turn (memory, summary, ...). Every role is
/// mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubAgentRole {
    // --- Task-stage responders (one per FSM stage; orchestrator routes) ---
    /// Gathers requirements and produces an approved plan (Clarify folds in here).
    Planning,
    /// Executes the approved plan within the current step only.
    Execution,
    /// Validates the result (tests / review / plan conformance).
    Validation,
    /// Records the final result. Terminal stage.
    Done,
    /// Default responder for plain chat that has no active task.
    General,
    // --- Service agents (maintain the turn, not responders) ---
    /// Extracts KV facts and routes them to memory layers. Runs every strategy.
    Memory,
    /// Compacts old history into a running summary.
    Summary,
    /// Routes a message to a scoped topic/branch.
    Topic,
    /// Fills the agent's long-term interview profile from the dialog.
    Profile,
    /// Checks responses against the agent's invariants (Pass/Fail → retry).
    Invariant,
    /// Dynamic pipeline worker. Workers are configured per task stage and run
    /// as real provider sub-requests before the stage responder.
    Worker,
    /// Legacy role kept only so old persisted configs still deserialize. Not in
    /// [`SubAgentRole::ALL`]; superseded by the stage responders + orchestrator.
    Task,
}

impl SubAgentRole {
    /// Every mandatory role, in stable order (defaults, resolution, UI listing).
    /// Legacy `Task` is intentionally excluded.
    pub const ALL: [SubAgentRole; 10] = [
        SubAgentRole::Planning,
        SubAgentRole::Execution,
        SubAgentRole::Validation,
        SubAgentRole::Done,
        SubAgentRole::General,
        SubAgentRole::Memory,
        SubAgentRole::Summary,
        SubAgentRole::Topic,
        SubAgentRole::Profile,
        SubAgentRole::Invariant,
    ];

    /// The four task-stage responder roles, in FSM order.
    pub const STAGES: [SubAgentRole; 4] = [
        SubAgentRole::Planning,
        SubAgentRole::Execution,
        SubAgentRole::Validation,
        SubAgentRole::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SubAgentRole::Planning => "planning",
            SubAgentRole::Execution => "execution",
            SubAgentRole::Validation => "validation",
            SubAgentRole::Done => "done",
            SubAgentRole::General => "general",
            SubAgentRole::Memory => "memory",
            SubAgentRole::Summary => "summary",
            SubAgentRole::Topic => "topic",
            SubAgentRole::Profile => "profile",
            SubAgentRole::Invariant => "invariant",
            SubAgentRole::Worker => "worker",
            SubAgentRole::Task => "task",
        }
    }

    /// Whether this role is a task-stage responder.
    pub fn is_stage(self) -> bool {
        matches!(
            self,
            SubAgentRole::Planning
                | SubAgentRole::Execution
                | SubAgentRole::Validation
                | SubAgentRole::Done
        )
    }
}

impl fmt::Display for SubAgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SubAgentRole {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "planning" => Ok(SubAgentRole::Planning),
            "execution" => Ok(SubAgentRole::Execution),
            "validation" => Ok(SubAgentRole::Validation),
            "done" => Ok(SubAgentRole::Done),
            "general" => Ok(SubAgentRole::General),
            "memory" => Ok(SubAgentRole::Memory),
            "summary" => Ok(SubAgentRole::Summary),
            "topic" => Ok(SubAgentRole::Topic),
            "profile" => Ok(SubAgentRole::Profile),
            "invariant" => Ok(SubAgentRole::Invariant),
            "worker" => Ok(SubAgentRole::Worker),
            "task" => Ok(SubAgentRole::Task),
            _ => Err(()),
        }
    }
}

// Serialize a role as a plain string (like `MemoryLayer`), so TOON stores it as
// a scalar value.
impl Serialize for SubAgentRole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SubAgentRole {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        SubAgentRole::from_str(&raw).map_err(|_| serde::de::Error::custom("invalid sub-agent role"))
    }
}

/// Persisted, token-free config for one mandatory sub-agent. Empty override
/// strings mean "inherit the main agent's provider/model"; empty
/// `system_prompt` means "use the role's built-in default prompt".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentConfig {
    pub role: SubAgentRole,
    /// Legacy compatibility field. A swarm role is mandatory; persisted
    /// `enabled=false` values are normalized back to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    /// Private memory namespace for this agent. Empty ⇒ shares the agent's main
    /// memory store; non-empty ⇒ the agent reads/writes its own isolated facts +
    /// summary partition, exchanging knowledge only through the orchestrator.
    #[serde(default)]
    pub memory_scope: String,
}

fn default_true() -> bool {
    true
}

impl SubAgentConfig {
    /// All-inherit, mandatory config for a role.
    pub fn inherit(role: SubAgentRole) -> Self {
        Self {
            role,
            enabled: true,
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            system_prompt: String::new(),
            memory_scope: String::new(),
        }
    }

    /// True when no provider/model override is set (sub-request reuses the main
    /// agent's credentials, only the model may differ if `model` is set).
    pub fn inherits_provider(&self) -> bool {
        self.provider.trim().is_empty() && self.base_url.trim().is_empty()
    }
}

/// The full mandatory swarm: one config per logical chain. Persisted with
/// `SavedAgent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmConfig {
    #[serde(default)]
    pub agents: Vec<SubAgentConfig>,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

impl SwarmConfig {
    /// All six sub-agents, inheriting the main agent.
    pub fn defaults() -> Self {
        Self {
            agents: SubAgentRole::ALL
                .iter()
                .copied()
                .map(SubAgentConfig::inherit)
                .collect(),
        }
    }

    /// Config for a role, falling back to an inherit default if the persisted
    /// list omits it (forward/backward compatible).
    pub fn get(&self, role: SubAgentRole) -> SubAgentConfig {
        let mut config = self
            .agents
            .iter()
            .find(|agent| agent.role == role)
            .cloned()
            .unwrap_or_else(|| SubAgentConfig::inherit(role));
        config.enabled = true;
        config
    }

    pub fn is_enabled(&self, role: SubAgentRole) -> bool {
        let _ = role;
        true
    }

    /// Upsert a role's config (used by the web API when the user edits one row).
    pub fn set(&mut self, mut config: SubAgentConfig) {
        config.enabled = true;
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|agent| agent.role == config.role)
        {
            *existing = config;
        } else {
            self.agents.push(config);
        }
    }

    /// Ensure every role is present (fills gaps with inherit defaults). Keeps a
    /// loaded old `SavedAgent` complete after deserialization.
    pub fn normalized(mut self) -> Self {
        for agent in &mut self.agents {
            agent.enabled = true;
        }
        for role in SubAgentRole::ALL {
            if !self.agents.iter().any(|agent| agent.role == role) {
                self.agents.push(SubAgentConfig::inherit(role));
            }
        }
        self
    }
}
