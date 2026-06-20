//! Core of the swarm: the [`SubAgent`] trait (every agent is a first-class
//! entity — "agent = class"), the per-turn mutable context [`SwarmTurn`], and the
//! structured [`SubAgentOutcome`] agents return to the deterministic orchestrator.

use async_trait::async_trait;
use tokio::time::{Duration, timeout};

use crate::{
    config::ProfileConfig,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ChatResponse, ModelPricing, ProviderClient,
        ProviderExchangeDebug, RequestMetrics, ResponseControl, Role,
    },
};

use super::super::memory::{AgentMemory, MemoryConfig};
use super::super::store::{AgentProfile, TaskArtifact, TaskContext, TopicFileStorage, UserProfile};
use super::config::SubAgentRole;
use super::runtime::ResolvedSwarm;
use super::{SwarmReport, SwarmRunRecord};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InvariantCheckStatus {
    #[default]
    NotRun,
    Passed,
    Failed,
    /// Some invariants are not deterministically checkable in code. The verifiable
    /// ones passed; the rest are carried in the prompt as guidance. Advisory is
    /// NOT a block — it never withholds the answer.
    Advisory,
    /// Retained for serialized back-compat; the code-only invariant check no
    /// longer emits this. Treated like `Advisory` by the UI.
    Unavailable,
}

/// What one agent produced this turn. The orchestrator (pure code) reads this to
/// decide routing — agents never name a target stage themselves.
#[derive(Debug, Clone, Default)]
pub struct SubAgentOutcome {
    /// User-facing answer, when this agent is the turn's responder.
    pub answer: Option<String>,
    /// Responder signals its stage's work is complete (orchestrator advances the
    /// FSM deterministically via the allowed-next table).
    pub stage_complete: bool,
    /// Optional artifact (key, value) produced by a stage.
    pub artifact: Option<(String, String)>,
    /// Invariant violations found (Invariant agent only).
    pub violations: Vec<String>,
    /// Whether the semantic invariant check passed, failed, or could not be
    /// trusted. `Unavailable` is fail-closed for locally unknown constraints.
    pub invariant_status: InvariantCheckStatus,
    /// When set with `answer`, this answer is terminal for the turn (e.g. the
    /// Topic agent could not route and returns a canned message).
    pub short_circuit: bool,
    /// Provider metrics for this agent's sub-request, if any.
    pub metrics: Option<RequestMetrics>,
}

impl SubAgentOutcome {
    pub fn answer(text: impl Into<String>, metrics: Option<RequestMetrics>) -> Self {
        Self {
            answer: Some(text.into()),
            metrics,
            ..Self::default()
        }
    }
}

/// Mutable context for a single user turn, shared across the swarm. Holds direct
/// field borrows (built by `ChatAgent`) so agents are decoupled entities rather
/// than methods on `ChatAgent`.
pub struct SwarmTurn<'a> {
    pub client: &'a dyn ProviderClient,
    pub roster: &'a ResolvedSwarm,
    /// Main agent provider/model/token (responder default + fallback).
    pub main_profile: &'a ProfileConfig,
    pub main_token: &'a str,
    pub control: &'a ResponseControl,
    pub pricing: &'a Option<ModelPricing>,
    pub billing: &'a Option<BillingLookup>,
    pub context_limit: Option<u32>,
    pub topic_store: Option<&'a TopicFileStorage>,

    pub memory_config: &'a mut MemoryConfig,
    pub memory: &'a mut AgentMemory,
    pub history: &'a mut Vec<ChatMessage>,
    pub task: &'a mut Option<TaskContext>,
    pub agent_profile: &'a mut Option<AgentProfile>,
    pub user_profile: &'a Option<UserProfile>,
    pub invariants: &'a [String],
    pub domain: &'a str,

    /// Current user prompt for the turn.
    pub prompt: &'a str,
    /// Responder's answer awaiting invariant validation (before commit).
    pub pending_answer: Option<String>,
    /// Invariant violations fed back into a responder retry (also covers plain
    /// chat without a task).
    pub retry_violations: Vec<String>,
    /// Fresh outputs from real pipeline worker sub-requests. The stage responder
    /// receives these as read-only evidence and they are persisted as artifacts.
    pub worker_outputs: Vec<TaskArtifact>,
    /// Aggregate auxiliary token metrics across all swarm sub-requests.
    pub aux_metrics: Option<RequestMetrics>,
    /// Provider HTTP debug of the main responder request, for the web Debug tab.
    pub captured_debug: Option<ProviderExchangeDebug>,
    /// Per-turn report of swarm activity, surfaced to the UI.
    pub report: SwarmReport,
    /// Memory scope of the agent currently responding this turn. Empty ⇒ shared
    /// store; non-empty ⇒ facts read/written go to that agent's private partition.
    /// Set by the orchestrator before the responder + memory agents run.
    pub active_scope: String,
}

/// Resolve a configured `memory_scope` string into the effective partition key.
///
/// Default is PRIVATE memory for the specialized swarm roles (stage responders +
/// service agents): an unset scope gives the agent its own role-named partition.
/// The `General` responder is the main agent's own voice, so it defaults to the
/// SHARED store. Either default can be overridden: a name forces a private scope,
/// the `shared` sentinel forces the shared store.
pub fn effective_memory_scope(role: SubAgentRole, configured: &str) -> String {
    let configured = configured.trim();
    if configured.eq_ignore_ascii_case("shared")
        || configured.eq_ignore_ascii_case("общая")
        || configured.eq_ignore_ascii_case("общий")
    {
        return String::new();
    }
    if !configured.is_empty() {
        return configured.to_string();
    }
    if role == SubAgentRole::General {
        return String::new();
    }
    role.as_str().to_string()
}

impl<'a> SwarmTurn<'a> {
    /// The effective private memory scope for `role`. Default is PRIVATE: an
    /// unset scope gives the agent its own role-named partition. The explicit
    /// sentinel `shared` opts back into the common store.
    pub fn scope_for(&self, role: SubAgentRole) -> String {
        let configured = self
            .roster
            .for_role(role)
            .map(|agent| agent.memory_scope.trim().to_string())
            .unwrap_or_default();
        effective_memory_scope(role, &configured)
    }
    /// Run one service sub-request as a `[system, user]` exchange using the
    /// resolved provider/model for `role`. `default_prompt` is used unless the
    /// agent carries a custom system prompt. Records activity in the report.
    pub async fn sub_request(
        &mut self,
        role: SubAgentRole,
        default_prompt: &str,
        user_content: String,
        control: ResponseControl,
        request_timeout: Duration,
    ) -> Option<ChatResponse> {
        let (profile, token, system_prompt) = match self.roster.for_role(role) {
            Some(agent) => {
                let system_prompt = if agent.system_prompt.trim().is_empty() {
                    default_prompt.to_string()
                } else {
                    agent.system_prompt.clone()
                };
                (agent.profile.clone(), agent.token.clone(), system_prompt)
            }
            None => (
                self.main_profile.clone(),
                self.main_token.to_string(),
                default_prompt.to_string(),
            ),
        };
        let mut record = SwarmRunRecord::new(role, profile.model.clone());
        let request = ChatRequest {
            model: profile.model.clone(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: system_prompt,
                },
                ChatMessage {
                    role: Role::User,
                    content: user_content,
                },
            ],
            control,
            pricing: self.pricing.clone(),
            billing: self.billing.clone(),
        };
        let outcome = timeout(
            request_timeout,
            self.client.chat_completion(&profile, &token, request),
        )
        .await;
        match outcome {
            Ok(Ok(response)) => {
                record.ran = true;
                record.metrics = Some(response.metrics.clone());
                self.report.record(record);
                Some(response)
            }
            _ => {
                record.note = "no response".to_string();
                self.report.record(record);
                None
            }
        }
    }

    /// Resolve the (profile, token) a responder role should use for the MAIN
    /// answer. Falls back to the main agent if the role is somehow unresolved.
    pub fn responder_profile(&self, role: SubAgentRole) -> (ProfileConfig, String) {
        match self.roster.for_role(role) {
            Some(agent) => (agent.profile.clone(), agent.token.clone()),
            None => (self.main_profile.clone(), self.main_token.to_string()),
        }
    }
}

/// Every swarm agent is an entity implementing this trait. Service agents
/// maintain the turn; stage/general agents act as responders.
#[async_trait]
pub trait SubAgent: Send + Sync {
    fn role(&self) -> SubAgentRole;
    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome;
}
