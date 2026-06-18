use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::chat::swarm::SwarmConfig;
use crate::providers::{ChatMessage, Role};

/// Profile-shared long-term facts. Persisted separately from the per-session
/// memory sidecar so that durable knowledge survives into brand-new sessions.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LongTermMemory {
    #[serde(default)]
    pub facts: std::collections::BTreeMap<String, String>,
}

/// A user-created, persistent stateful agent. Owns its settings, its domain, its
/// invariants, plus two auto-populated memory layers: working (`TaskContext` with
/// a task-stage FSM) and long-term (`AgentProfile`, filled by the agent's own
/// interview). The short-term layer is the agent's chat session
/// (`session_key = "agent:<id>"`). The provider token is NOT stored here — it
/// stays in the keyring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedAgent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    /// Free-text domain description the user provides at creation. Seeds the
    /// interview-schema generation and is injected so the agent stays on-domain.
    #[serde(default)]
    pub domain: String,
    /// Hard constraints (stack, architecture, bans) that must not change between
    /// requests. Injected into the prompt and checked against each response.
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub memory: SavedMemoryConfig,
    /// Per-agent mandatory swarm: each logical chain (memory, summary, task,
    /// ...) as a first-class sub-agent with its own model + prompt. Defaults to
    /// all-on/inherit so old agents keep working.
    #[serde(default = "SwarmConfig::defaults")]
    pub swarm: SwarmConfig,
    #[serde(default)]
    pub created_at_unix: u64,
    #[serde(default)]
    pub updated_at_unix: u64,
}

/// Serializable subset of `MemoryConfig` (strategy as string), persisted with an
/// agent so its memory settings survive across sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedMemoryConfig {
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub recent_messages: usize,
    #[serde(default)]
    pub summarize_after_messages: usize,
    #[serde(default)]
    pub summary_chunk_messages: usize,
    #[serde(default)]
    pub summarize_at_context_percent: u8,
    #[serde(default)]
    pub summary_prompt: String,
    #[serde(default)]
    pub facts_extraction_prompt: String,
    #[serde(default)]
    pub facts_prompt: String,
    #[serde(default)]
    pub active_branch: String,
    #[serde(default)]
    pub scoped_auto_route: bool,
    #[serde(default)]
    pub topic_file_routing: bool,
    #[serde(default)]
    pub topic_drift_guard: bool,
    #[serde(default)]
    pub topic_auto_create: bool,
    #[serde(default)]
    pub topic_classifier_prompt: String,
}

/// Reusable user preferences, intentionally separate from `SavedAgent`.
/// A profile describes the person receiving answers; it does not define agent
/// identity, tools, workflow, or capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserProfile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub style_preferences: Vec<String>,
    #[serde(default)]
    pub format_preferences: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub language_preferences: Vec<String>,
    #[serde(default)]
    pub response_length: String,
    #[serde(default)]
    pub custom_instructions: String,
    #[serde(default)]
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserProfileBindings {
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub default_profile_per_agent: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct UserProfilesIndex {
    #[serde(default)]
    pub(super) profiles: Vec<UserProfile>,
}

/// Stage of the task state machine. Auto-detected by the agent from the dialog,
/// but transitions are validated in code (see [`TaskStage::can_transition`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskStage {
    /// Gathering / clarifying the task. Entry stage.
    #[default]
    Clarify,
    /// Designing an approved plan.
    Planning,
    /// Executing the plan.
    Execution,
    /// Validating the result (tests / review).
    Validation,
    /// Finished. Terminal stage.
    Done,
}

// Serialized as its Display string (not a serde enum) so the TOON codec can store
// it as a plain map value, mirroring `MemoryLayer`.
impl Serialize for TaskStage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TaskStage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(raw.parse().unwrap_or_default())
    }
}

impl TaskStage {
    /// Stages in canonical order, for stable rendering / UI.
    pub const ORDERED: [TaskStage; 5] = [
        TaskStage::Clarify,
        TaskStage::Planning,
        TaskStage::Execution,
        TaskStage::Validation,
        TaskStage::Done,
    ];

    /// Stages reachable from `self`. Empty for the terminal stage.
    pub fn allowed_next(self) -> &'static [TaskStage] {
        match self {
            TaskStage::Clarify => &[TaskStage::Planning],
            TaskStage::Planning => &[TaskStage::Execution],
            TaskStage::Execution => &[TaskStage::Validation, TaskStage::Planning],
            TaskStage::Validation => &[TaskStage::Done, TaskStage::Execution],
            TaskStage::Done => &[],
        }
    }

    /// Whether moving from `self` to `to` is allowed. Staying put is always allowed.
    pub fn can_transition(self, to: TaskStage) -> bool {
        self == to || self.allowed_next().contains(&to)
    }
}

impl std::fmt::Display for TaskStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            TaskStage::Clarify => "clarify",
            TaskStage::Planning => "planning",
            TaskStage::Execution => "execution",
            TaskStage::Validation => "validation",
            TaskStage::Done => "done",
        };
        f.write_str(label)
    }
}

impl std::str::FromStr for TaskStage {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "clarify" | "clarification" | "clarifying" => Ok(TaskStage::Clarify),
            "planning" | "plan" => Ok(TaskStage::Planning),
            "execution" | "execute" | "executing" | "exec" => Ok(TaskStage::Execution),
            "validation" | "validate" | "validating" | "review" => Ok(TaskStage::Validation),
            "done" | "finished" | "complete" | "completed" => Ok(TaskStage::Done),
            _ => Err(()),
        }
    }
}

/// Working memory: the agent's current task, shared across all of its sessions.
/// Auto-populated by the agent; `stage` is driven by the task state machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    #[serde(default)]
    pub stage: TaskStage,
    #[serde(default)]
    pub current_step: String,
    #[serde(default)]
    pub expected_action: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub resume_hint: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub goal: String,
    /// The approved plan (one entry per step).
    #[serde(default)]
    pub plan: Vec<String>,
    #[serde(default)]
    pub pipeline: Vec<TaskPipelineStage>,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifact>,
    /// Results / outputs accumulated as stages complete.
    #[serde(default)]
    pub results: Vec<String>,
    #[serde(default)]
    pub notes: String,
    /// Invariant violations found in the agent's last response. Surfaced in debug
    /// and injected on the next turn as corrective feedback (the code-validation
    /// loop). Cleared once a clean response passes.
    #[serde(default)]
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskPipelineStage {
    #[serde(default)]
    pub stage: TaskStage,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub worker_agents: Vec<TaskWorkerAgent>,
    #[serde(default)]
    pub artifact_key: String,
    #[serde(default)]
    pub requires_human_approval: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskWorkerAgent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub system_prompt: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtifact {
    #[serde(default)]
    pub stage: TaskStage,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineAdvance {
    pub from: TaskStage,
    pub to: TaskStage,
    pub accepted: bool,
    pub paused_for_human: bool,
}

impl TaskContext {
    /// Whether the task context has any content worth injecting beyond the
    /// default entry stage.
    pub fn is_empty(&self) -> bool {
        self.stage == TaskStage::Clarify
            && self.current_step.trim().is_empty()
            && self.expected_action.trim().is_empty()
            && !self.paused
            && self.resume_hint.trim().is_empty()
            && self.title.trim().is_empty()
            && self.goal.trim().is_empty()
            && self.notes.trim().is_empty()
            && self.plan.iter().all(|step| step.trim().is_empty())
            && self.pipeline.is_empty()
            && self.artifacts.is_empty()
            && self.results.iter().all(|item| item.trim().is_empty())
    }

    pub fn pause(&mut self, resume_hint: impl Into<String>) -> bool {
        if self.stage == TaskStage::Done {
            return false;
        }
        self.paused = true;
        let hint = resume_hint.into();
        if !hint.trim().is_empty() {
            self.resume_hint = hint;
        }
        true
    }

    pub fn resume(&mut self) -> bool {
        if !self.paused {
            return false;
        }
        self.paused = false;
        true
    }

    pub fn approve_pipeline_pause(&mut self) -> bool {
        if !self.paused {
            return false;
        }
        let current = self.stage;
        let Some(active_index) = self
            .pipeline
            .iter()
            .position(|stage| stage.stage == current && stage.requires_human_approval)
        else {
            return self.resume();
        };
        let next = self
            .pipeline
            .get(active_index + 1)
            .map(|stage| stage.stage)
            .unwrap_or(TaskStage::Done);
        if !current.can_transition(next) {
            return false;
        }
        self.stage = next;
        self.paused = false;
        if let Some(next_stage) = self.active_pipeline_stage() {
            let label = pipeline_stage_label(next_stage);
            self.current_step = label.clone();
            self.expected_action = "agent_work".to_string();
            self.resume_hint = format!("Run {label} stage after human approval");
        } else {
            self.current_step = "complete task".to_string();
            self.expected_action = "none".to_string();
            self.resume_hint = "Pipeline completed after human approval".to_string();
        }
        true
    }

    pub fn set_progress(
        &mut self,
        current_step: impl Into<String>,
        expected_action: impl Into<String>,
    ) {
        let current_step = current_step.into();
        if !current_step.trim().is_empty() {
            self.current_step = current_step;
        }
        let expected_action = expected_action.into();
        if !expected_action.trim().is_empty() {
            self.expected_action = expected_action;
        }
    }

    pub fn active_pipeline_stage(&self) -> Option<&TaskPipelineStage> {
        self.pipeline.iter().find(|stage| stage.stage == self.stage)
    }

    pub fn complete_pipeline_stage(
        &mut self,
        artifact_key: impl Into<String>,
        artifact_value: impl Into<String>,
    ) -> Option<PipelineAdvance> {
        let current = self.stage;
        let active_index = self
            .pipeline
            .iter()
            .position(|stage| stage.stage == current)?;
        let active = self.pipeline[active_index].clone();
        let key = artifact_key.into();
        let value = artifact_value.into();
        if !key.trim().is_empty() || !value.trim().is_empty() {
            self.artifacts.push(TaskArtifact {
                stage: current,
                key: if key.trim().is_empty() {
                    active.artifact_key.clone()
                } else {
                    key
                },
                value,
            });
        }

        if active.requires_human_approval {
            self.paused = true;
            self.current_step = format!("approve {} artifact", pipeline_stage_label(&active));
            self.expected_action = "user_input".to_string();
            self.resume_hint = format!(
                "Review {} artifact before moving past {}",
                active.artifact_key.trim().if_empty("stage output"),
                pipeline_stage_label(&active)
            );
            return Some(PipelineAdvance {
                from: current,
                to: current,
                accepted: true,
                paused_for_human: true,
            });
        }

        let next = self
            .pipeline
            .get(active_index + 1)
            .map(|stage| stage.stage)
            .unwrap_or(TaskStage::Done);
        let accepted = current.can_transition(next);
        if accepted {
            self.stage = next;
            self.paused = false;
            if let Some(next_stage) = self.active_pipeline_stage() {
                let label = pipeline_stage_label(next_stage);
                self.current_step = label.clone();
                self.expected_action = "agent_work".to_string();
                self.resume_hint = format!("Run {label} stage");
            } else {
                self.current_step = "complete task".to_string();
                self.expected_action = "none".to_string();
                self.resume_hint = "Pipeline completed".to_string();
            }
        }
        Some(PipelineAdvance {
            from: current,
            to: next,
            accepted,
            paused_for_human: false,
        })
    }
}

pub(super) fn pipeline_stage_label(stage: &TaskPipelineStage) -> String {
    let name = stage.name.trim();
    if name.is_empty() {
        stage.stage.to_string()
    } else {
        name.to_string()
    }
}

trait IfEmpty {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl IfEmpty for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

/// One field of the agent's interview schema. `value` empty means the agent has
/// not yet elicited it from the user.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileField {
    #[serde(default)]
    pub key: String,
    /// The question the agent asks the user to fill this field.
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub value: String,
}

impl ProfileField {
    pub fn is_filled(&self) -> bool {
        !self.value.trim().is_empty()
    }
}

/// Long-term memory: the agent's profile — a schema of fields (stack, audience,
/// constraints, …) the agent interviews the user to fill. Stored per-agent and
/// shared across sessions. Replaces the old free-text `KnowledgeDoc`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(default)]
    pub fields: Vec<ProfileField>,
    #[serde(default)]
    pub updated_at_unix: u64,
}

impl AgentProfile {
    /// Required fields the agent has not yet filled — what it still needs to ask.
    pub fn pending_required(&self) -> Vec<&ProfileField> {
        self.fields
            .iter()
            .filter(|field| field.required && !field.is_filled())
            .collect()
    }
}

/// Lightweight entry for the agents index / picker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// Current task stage, surfaced in the agent picker.
    #[serde(default)]
    pub stage: TaskStage,
    #[serde(default)]
    pub updated_at_unix: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct AgentsIndex {
    #[serde(default)]
    pub(super) agents: Vec<AgentSummary>,
}

/// One chat (dialog) belonging to an agent. An agent can have many; they all
/// can share a working task while keeping separate chat history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DialogMeta {
    #[serde(default)]
    pub id: String,
    /// Working task / feature this dialog belongs to. Multiple dialogs may point
    /// at the same task id.
    #[serde(default = "default_task_id")]
    pub task_id: String,
    /// Short label, derived from the first user message (or renamed by the user).
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created_at_unix: u64,
    #[serde(default)]
    pub updated_at_unix: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct DialogsIndex {
    #[serde(default)]
    pub(super) dialogs: Vec<DialogMeta>,
}

pub(super) fn default_task_id() -> String {
    "default".to_string()
}

pub(super) fn blank_str_to_none(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

/// Extract the agent id from a session key of the form `agent:<id>`.
pub fn agent_id_from_key(session_key: &str) -> Option<&str> {
    session_key
        .strip_prefix("agent:")
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Derive a short dialog title from the first user message in a history.
pub(super) fn dialog_title_from_messages(messages: &[ChatMessage]) -> String {
    let first = messages
        .iter()
        .find(|message| message.role == Role::User)
        .map(|message| message.content.trim())
        .unwrap_or_default();
    let one_line = first.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 48;
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        let truncated: String = one_line.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
