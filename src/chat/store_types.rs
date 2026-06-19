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
        raw.parse()
            .map_err(|_| serde::de::Error::custom(format!("invalid task stage: {raw}")))
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

    /// The normal forward lifecycle edge. Backward repair transitions remain in
    /// [`Self::allowed_next`] but are never selected by stage completion.
    pub fn canonical_next(self) -> Option<TaskStage> {
        match self {
            TaskStage::Clarify => Some(TaskStage::Planning),
            TaskStage::Planning => Some(TaskStage::Execution),
            TaskStage::Execution => Some(TaskStage::Validation),
            TaskStage::Validation => Some(TaskStage::Done),
            TaskStage::Done => None,
        }
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

/// Why a task is paused. Approval pauses are intentionally distinct from a
/// manual pause so ordinary follow-up text cannot accidentally approve a gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskPauseReason {
    #[default]
    None,
    Manual,
    PlanApproval,
    StageApproval,
}

impl Serialize for TaskPauseReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TaskPauseReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse()
            .map_err(|_| serde::de::Error::custom(format!("invalid task pause reason: {raw}")))
    }
}

impl std::fmt::Display for TaskPauseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TaskPauseReason::None => "none",
            TaskPauseReason::Manual => "manual",
            TaskPauseReason::PlanApproval => "plan_approval",
            TaskPauseReason::StageApproval => "stage_approval",
        })
    }
}

impl std::str::FromStr for TaskPauseReason {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Ok(TaskPauseReason::None),
            "manual" => Ok(TaskPauseReason::Manual),
            "plan_approval" | "plan-approval" => Ok(TaskPauseReason::PlanApproval),
            "stage_approval" | "stage-approval" | "human_approval" => {
                Ok(TaskPauseReason::StageApproval)
            }
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTransitionDecision {
    pub from: TaskStage,
    pub to: TaskStage,
    pub accepted: bool,
    pub reason: String,
}

/// Working memory: the agent's current task, shared across all of its sessions.
/// Auto-populated by the agent; `stage` is driven by the task state machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    /// Optimistic-concurrency revision. Storage increments it on each successful
    /// save and rejects stale writers.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub stage: TaskStage,
    #[serde(default)]
    pub current_step: String,
    #[serde(default)]
    pub expected_action: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub pause_reason: TaskPauseReason,
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
    pub plan_approved: bool,
    #[serde(default)]
    pub validation_passed: bool,
    /// Last lifecycle rejection, safe to show in the UI and inject as working
    /// context. Cleared after a successful transition.
    #[serde(default)]
    pub transition_block_reason: String,
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
    /// Other tasks tracked in parallel with this (active) one. Each is paused and
    /// keeps its own stage/plan/results so work is never lost when the user
    /// switches between tasks. Backlog entries never nest a backlog of their own.
    #[serde(default)]
    pub backlog: Vec<TaskContext>,
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
            && self.pause_reason == TaskPauseReason::None
            && self.resume_hint.trim().is_empty()
            && self.title.trim().is_empty()
            && self.goal.trim().is_empty()
            && self.notes.trim().is_empty()
            && self.plan.iter().all(|step| step.trim().is_empty())
            && !self.plan_approved
            && !self.validation_passed
            && self.transition_block_reason.trim().is_empty()
            && self.pipeline.is_empty()
            && self.artifacts.is_empty()
            && self.results.iter().all(|item| item.trim().is_empty())
            && self.backlog.is_empty()
    }

    /// Park the current active task and promote a fresh `Clarify` task driven by
    /// `goal`. The parked task is paused (work preserved) and pushed to the
    /// backlog so it can be resumed later. Called by the orchestrator when the
    /// user clearly starts a new task — including from the terminal `Done` stage.
    pub fn start_new_task(&mut self, goal: impl Into<String>, resume_hint: impl Into<String>) {
        let goal = goal.into();
        let document_revision = self.revision;
        let mut backlog = std::mem::take(&mut self.backlog);
        let mut parked = std::mem::take(self);
        self.revision = document_revision;
        parked.revision = 0;
        // Flatten: backlog entries never carry their own backlog.
        parked.backlog.clear();
        if !parked.is_empty() {
            if parked.stage != TaskStage::Done {
                parked.pause(resume_hint);
            }
            backlog.push(parked);
        }
        self.backlog = backlog;
        self.goal = goal;
    }

    /// Swap the active task with backlog entry `index`, resuming it. The currently
    /// active task is paused and returned to the backlog so nothing is lost.
    pub fn switch_to_backlog(&mut self, index: usize) -> bool {
        if index >= self.backlog.len() {
            return false;
        }
        let document_revision = self.revision;
        let mut incoming = self.backlog.remove(index);
        incoming.revision = document_revision;
        incoming.resume();
        let mut backlog = std::mem::take(&mut self.backlog);
        let mut outgoing = std::mem::replace(self, incoming);
        outgoing.revision = 0;
        outgoing.backlog.clear();
        if !outgoing.is_empty() {
            if outgoing.stage != TaskStage::Done {
                outgoing.pause(String::new());
            }
            backlog.push(outgoing);
        }
        self.backlog = backlog;
        true
    }

    pub fn pause(&mut self, resume_hint: impl Into<String>) -> bool {
        if self.stage == TaskStage::Done {
            return false;
        }
        self.paused = true;
        self.pause_reason = TaskPauseReason::Manual;
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
        self.pause_reason = TaskPauseReason::None;
        self.transition_block_reason.clear();
        true
    }

    pub fn pause_for(&mut self, reason: TaskPauseReason, resume_hint: impl Into<String>) -> bool {
        if self.stage == TaskStage::Done || reason == TaskPauseReason::None {
            return false;
        }
        self.paused = true;
        self.pause_reason = reason;
        let hint = resume_hint.into();
        if !hint.trim().is_empty() {
            self.resume_hint = hint;
        }
        true
    }

    pub fn has_stage_artifact(&self, stage: TaskStage) -> bool {
        self.artifacts
            .iter()
            .any(|artifact| artifact.stage == stage && !artifact.value.trim().is_empty())
    }

    pub fn record_stage_artifact(
        &mut self,
        stage: TaskStage,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> bool {
        let key = key.into();
        let value = value.into();
        if value.trim().is_empty() {
            self.transition_block_reason =
                format!("Стадия {stage} не завершена: результат пуст.").to_string();
            return false;
        }
        self.artifacts.push(TaskArtifact { stage, key, value });
        true
    }

    pub fn validate_pipeline(&self) -> Result<(), String> {
        if self.pipeline.is_empty() {
            return Ok(());
        }
        if self.pipeline.len() != 3 {
            return Err(
                "Pipeline должен содержать ровно planning, execution и validation.".to_string(),
            );
        }
        let mut previous: Option<TaskStage> = None;
        for stage in &self.pipeline {
            if stage.stage == TaskStage::Clarify || stage.stage == TaskStage::Done {
                return Err(format!(
                    "Pipeline stage {} нельзя настраивать как рабочую стадию.",
                    stage.stage
                ));
            }
            if stage.artifact_key.trim().is_empty() {
                return Err(format!(
                    "У стадии {} должен быть непустой artifact_key.",
                    stage.stage
                ));
            }
            if let Some(previous) = previous {
                if previous.canonical_next() != Some(stage.stage) {
                    return Err(format!(
                        "Pipeline должен идти по порядку planning → execution → validation; найдено {} → {}.",
                        previous, stage.stage
                    ));
                }
            } else if stage.stage != TaskStage::Planning {
                return Err("Pipeline должен начинаться со стадии planning.".to_string());
            }
            let mut worker_ids = std::collections::BTreeSet::new();
            for worker in &stage.worker_agents {
                let id = worker.id.trim();
                if id.is_empty() {
                    return Err(format!(
                        "У каждого worker стадии {} должен быть непустой id.",
                        stage.stage
                    ));
                }
                if !worker_ids.insert(id) {
                    return Err(format!(
                        "Worker id `{id}` повторяется на стадии {}.",
                        stage.stage
                    ));
                }
            }
            previous = Some(stage.stage);
        }
        if previous != Some(TaskStage::Validation) {
            return Err("Pipeline должен завершаться стадией validation.".to_string());
        }
        Ok(())
    }

    pub fn transition_requirement(&self, to: TaskStage) -> Result<(), String> {
        if !self.stage.can_transition(to) {
            return Err(format!(
                "Переход {} → {} запрещён. Допустимо: {}.",
                self.stage,
                to,
                self.stage
                    .allowed_next()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        match (self.stage, to) {
            (TaskStage::Planning, TaskStage::Execution) => {
                if !self.has_stage_artifact(TaskStage::Planning) {
                    return Err("Сначала нужен сохранённый результат стадии planning.".to_string());
                }
                if !self.plan_approved {
                    return Err(
                        "Сначала пользователь должен явно утвердить подготовленный план."
                            .to_string(),
                    );
                }
            }
            (TaskStage::Execution, TaskStage::Validation)
                if !self.has_stage_artifact(TaskStage::Execution) =>
            {
                return Err("Сначала нужен сохранённый результат стадии execution.".to_string());
            }
            (TaskStage::Validation, TaskStage::Done) => {
                if !self.has_stage_artifact(TaskStage::Validation) {
                    return Err(
                        "Финал запрещён: отсутствует результат стадии validation.".to_string()
                    );
                }
                if !self.validation_passed {
                    return Err(
                        "Финал запрещён: validation ещё не завершилась успешно.".to_string()
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn next_stage(&self) -> Option<TaskStage> {
        self.stage.canonical_next()
    }

    pub fn next_transition_requirement(&self) -> String {
        if self.paused {
            return match self.pause_reason {
                TaskPauseReason::PlanApproval => {
                    "Явное сообщение пользователя: «утверждаю план».".to_string()
                }
                TaskPauseReason::StageApproval => {
                    "Явное сообщение пользователя: «утверждаю результат».".to_string()
                }
                TaskPauseReason::Manual => "Явное сообщение пользователя: «продолжай».".to_string(),
                TaskPauseReason::None => "Снять паузу.".to_string(),
            };
        }
        match self.stage {
            TaskStage::Clarify => {
                "PlanningAgent должен завершить уточнение и выдать непустой результат.".to_string()
            }
            TaskStage::Planning if !self.has_stage_artifact(TaskStage::Planning) => {
                "PlanningAgent должен подготовить и сохранить план.".to_string()
            }
            TaskStage::Planning if !self.plan_approved => {
                "Пользователь должен явно написать «утверждаю план».".to_string()
            }
            TaskStage::Planning => "План утверждён; разрешён переход в execution.".to_string(),
            TaskStage::Execution => {
                "ExecutionAgent должен создать непустой execution-артефакт.".to_string()
            }
            TaskStage::Validation if !self.validation_passed => {
                "ValidationAgent должен проверить результат и завершить validation успешно."
                    .to_string()
            }
            TaskStage::Validation => "Validation пройдена; разрешён переход в done.".to_string(),
            TaskStage::Done => "Задача завершена; исходящих переходов нет.".to_string(),
        }
    }

    /// Repair impossible persisted combinations before any responder runs.
    /// This protects upgraded/legacy task files and corrupted external state:
    /// work is rewound to the earliest stage whose prerequisite is missing.
    pub fn repair_lifecycle_integrity(&mut self) -> Option<String> {
        let target = match self.stage {
            TaskStage::Execution
                if !self.plan_approved || !self.has_stage_artifact(TaskStage::Planning) =>
            {
                Some((
                    TaskStage::Planning,
                    "Execution была сохранена без утверждённого plan artifact.",
                ))
            }
            TaskStage::Validation
                if !self.plan_approved || !self.has_stage_artifact(TaskStage::Planning) =>
            {
                Some((
                    TaskStage::Planning,
                    "Validation была сохранена без утверждённого плана.",
                ))
            }
            TaskStage::Validation if !self.has_stage_artifact(TaskStage::Execution) => Some((
                TaskStage::Execution,
                "Validation была сохранена без execution artifact.",
            )),
            TaskStage::Done
                if !self.plan_approved || !self.has_stage_artifact(TaskStage::Planning) =>
            {
                Some((
                    TaskStage::Planning,
                    "Done была сохранена без утверждённого плана.",
                ))
            }
            TaskStage::Done if !self.has_stage_artifact(TaskStage::Execution) => Some((
                TaskStage::Execution,
                "Done была сохранена без execution artifact.",
            )),
            TaskStage::Done
                if !self.validation_passed || !self.has_stage_artifact(TaskStage::Validation) =>
            {
                Some((
                    TaskStage::Validation,
                    "Done была сохранена без успешного validation artifact.",
                ))
            }
            _ => None,
        };
        let (target, cause) = target?;
        let from = self.stage;
        self.stage = target;
        self.paused = false;
        self.pause_reason = TaskPauseReason::None;
        if target == TaskStage::Planning {
            self.plan_approved = false;
            self.validation_passed = false;
        } else if target == TaskStage::Execution {
            self.validation_passed = false;
        }
        self.sync_progress_for_stage();
        let reason = format!(
            "Обнаружено неконсистентное состояние `{from}`: {cause} Задача безопасно возвращена в `{target}`."
        );
        self.transition_block_reason = reason.clone();
        Some(reason)
    }

    pub fn try_transition(&mut self, to: TaskStage) -> TaskTransitionDecision {
        let from = self.stage;
        match self.transition_requirement(to) {
            Ok(()) => {
                self.stage = to;
                self.paused = false;
                self.pause_reason = TaskPauseReason::None;
                self.transition_block_reason.clear();
                if matches!((from, to), (TaskStage::Execution, TaskStage::Planning)) {
                    self.plan_approved = false;
                    self.validation_passed = false;
                } else if matches!((from, to), (TaskStage::Validation, TaskStage::Execution)) {
                    self.validation_passed = false;
                }
                self.sync_progress_for_stage();
                TaskTransitionDecision {
                    from,
                    to,
                    accepted: true,
                    reason: String::new(),
                }
            }
            Err(reason) => {
                self.transition_block_reason = reason.clone();
                TaskTransitionDecision {
                    from,
                    to,
                    accepted: false,
                    reason,
                }
            }
        }
    }

    pub fn sync_progress_for_stage(&mut self) {
        let (step, action, hint) = match self.stage {
            TaskStage::Clarify => (
                "уточнить задачу",
                "agent_work",
                "Ответьте на вопросы Clarify/Planning агента.",
            ),
            TaskStage::Planning => (
                "подготовить утверждаемый план",
                "agent_work",
                "PlanningAgent должен выдать план; затем потребуется «утверждаю план».",
            ),
            TaskStage::Execution => (
                "выполнить утверждённый план",
                "agent_work",
                "ExecutionAgent создаёт результат только по утверждённому плану.",
            ),
            TaskStage::Validation => (
                "проверить результат",
                "validation",
                "ValidationAgent должен завершить проверку до финала.",
            ),
            TaskStage::Done => (
                "задача завершена",
                "none",
                "Для новой работы создайте новую задачу.",
            ),
        };
        self.current_step = step.to_string();
        self.expected_action = action.to_string();
        self.resume_hint = hint.to_string();
    }

    pub fn approve_pipeline_pause(&mut self) -> bool {
        if !self.paused {
            return false;
        }
        let current = self.stage;
        let active_index = self
            .pipeline
            .iter()
            .position(|stage| stage.stage == current);
        let next = active_index
            .and_then(|index| self.pipeline.get(index + 1))
            .map(|stage| stage.stage)
            .or_else(|| current.canonical_next())
            .unwrap_or(TaskStage::Done);
        if self.pause_reason == TaskPauseReason::PlanApproval {
            self.plan_approved = true;
        }
        let decision = self.try_transition(next);
        if !decision.accepted {
            return false;
        }
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

        if current == TaskStage::Validation {
            self.validation_passed = true;
        }

        if current == TaskStage::Planning || active.requires_human_approval {
            let pause_reason = if current == TaskStage::Planning {
                TaskPauseReason::PlanApproval
            } else {
                TaskPauseReason::StageApproval
            };
            self.paused = true;
            self.pause_reason = pause_reason;
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
        let decision = self.try_transition(next);
        let accepted = decision.accepted;
        if accepted {
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
