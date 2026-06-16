use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use uuid::Uuid;

use serde::{Deserialize, Serialize};

use crate::{
    errors::AppError,
    providers::{ChatMessage, RequestCost, RequestMetrics, Role, TokenUsage},
};

use super::memory::{AgentMemory, MemoryLayer, TopicFile};

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
struct UserProfilesIndex {
    #[serde(default)]
    profiles: Vec<UserProfile>,
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
    pub title: String,
    #[serde(default)]
    pub goal: String,
    /// The approved plan (one entry per step).
    #[serde(default)]
    pub plan: Vec<String>,
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

impl TaskContext {
    /// Whether the task context has any content worth injecting beyond the
    /// default entry stage.
    pub fn is_empty(&self) -> bool {
        self.stage == TaskStage::Clarify
            && self.title.trim().is_empty()
            && self.goal.trim().is_empty()
            && self.notes.trim().is_empty()
            && self.plan.iter().all(|step| step.trim().is_empty())
            && self.results.iter().all(|item| item.trim().is_empty())
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
struct AgentsIndex {
    #[serde(default)]
    agents: Vec<AgentSummary>,
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
struct DialogsIndex {
    #[serde(default)]
    dialogs: Vec<DialogMeta>,
}

fn default_task_id() -> String {
    "default".to_string()
}

fn blank_str_to_none(value: &str) -> Option<&str> {
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
fn dialog_title_from_messages(messages: &[ChatMessage]) -> String {
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

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSession {
    pub id: String,
    pub messages: Vec<ChatMessage>,
    pub metrics: RequestMetrics,
}

#[derive(Debug, Clone)]
pub struct LocalSessionStore {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TopicFileStorage {
    root: PathBuf,
    session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRequestMetrics {
    elapsed_ms: u128,
    usage: Option<TokenUsage>,
    cost: Option<StoredRequestCost>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRequestCost {
    amount: f64,
    currency: String,
    source: String,
}

impl LocalSessionStore {
    pub fn new() -> Result<Self, AppError> {
        let dirs = ProjectDirs::from("dev", "ai-playground", "ai-playground").ok_or_else(|| {
            AppError::Config {
                path: PathBuf::from("<unknown>"),
                message: "Could not resolve data directory".to_string(),
            }
        })?;
        Ok(Self {
            root: dirs.data_local_dir().join("history").join("sessions"),
        })
    }

    pub fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn load_or_create_latest(
        &self,
        profile_key: &str,
    ) -> Result<ConversationSession, AppError> {
        if let Some(session) = self.load_latest(profile_key)? {
            return Ok(session);
        }
        self.create_session()
    }

    pub fn load_latest(&self, profile_key: &str) -> Result<Option<ConversationSession>, AppError> {
        let path = self.index_path(profile_key);
        if !path.exists() {
            return Ok(None);
        }
        let session_id = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?
            .trim()
            .to_string();
        if session_id.is_empty() {
            return Ok(None);
        }
        self.load_session(&session_id).map(Some)
    }

    pub fn create_session(&self) -> Result<ConversationSession, AppError> {
        Ok(ConversationSession {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            metrics: RequestMetrics::default(),
        })
    }

    pub fn load_session(&self, session_id: &str) -> Result<ConversationSession, AppError> {
        validate_session_id(session_id)?;
        let path = self.session_path(session_id);
        if !path.exists() {
            let legacy_path = self.legacy_session_path(session_id);
            if legacy_path.exists() {
                return self.load_legacy_jsonl_session(session_id, &legacy_path);
            }
            return Ok(ConversationSession {
                id: session_id.to_string(),
                messages: Vec::new(),
                metrics: RequestMetrics::default(),
            });
        }

        let raw = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?;
        let messages = stored_messages_into_chat(crate::toon_codec::from_str_or_json::<
            Vec<StoredChatMessage>,
        >(&raw)?)?;
        Ok(ConversationSession {
            id: session_id.to_string(),
            messages,
            metrics: self.load_metrics(session_id)?,
        })
    }

    pub fn load_metrics(&self, session_id: &str) -> Result<RequestMetrics, AppError> {
        validate_session_id(session_id)?;
        let path = self.metrics_path(session_id);
        if !path.exists() {
            let legacy_path = self.legacy_metrics_path(session_id);
            if legacy_path.exists() {
                return self.load_legacy_json_metrics(&legacy_path);
            }
            return Ok(RequestMetrics::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?;
        stored_metrics_into_request(crate::toon_codec::from_str_or_json::<StoredRequestMetrics>(
            &raw,
        )?)
    }

    pub fn load_memory(&self, session_id: &str) -> Result<AgentMemory, AppError> {
        validate_session_id(session_id)?;
        let path = self.memory_path(session_id);
        if !path.exists() {
            return Ok(AgentMemory::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?;
        crate::toon_codec::from_str_or_json::<AgentMemory>(&raw)
    }

    pub fn save_session(
        &self,
        profile_key: &str,
        session_id: &str,
        messages: &[ChatMessage],
    ) -> Result<(), AppError> {
        validate_session_id(session_id)?;
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            config_error(
                self.sessions_dir(),
                format!("could not create directory: {error}"),
            )
        })?;
        fs::create_dir_all(self.index_dir()).map_err(|error| {
            config_error(
                self.index_dir(),
                format!("could not create directory: {error}"),
            )
        })?;

        let path = self.session_path(session_id);
        let temp_path = path.with_extension("toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(&stored_messages_from_chat(messages))?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            config_error(
                path.clone(),
                format!("could not replace session file: {error}"),
            )
        })?;
        fs::write(self.index_path(profile_key), session_id).map_err(|error| {
            config_error(
                self.index_path(profile_key),
                format!("could not write session index: {error}"),
            )
        })?;
        // For an agent's session, also register it in the agent's dialog index so
        // the UI can list and switch between the agent's chats. Title is seeded
        // from the first user message; later turns keep it.
        if let Some(agent_id) = agent_id_from_key(profile_key) {
            let title = dialog_title_from_messages(messages);
            self.register_dialog(agent_id, session_id, &title)?;
        }
        Ok(())
    }

    pub fn save_memory(&self, session_id: &str, memory: &AgentMemory) -> Result<(), AppError> {
        validate_session_id(session_id)?;
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            config_error(
                self.sessions_dir(),
                format!("could not create directory: {error}"),
            )
        })?;

        let path = self.memory_path(session_id);
        let temp_path = path.with_extension("memory.toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(memory)?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            config_error(
                path.clone(),
                format!("could not replace memory file: {error}"),
            )
        })?;
        Ok(())
    }

    /// Persist the long-term facts of `memory` into the profile-shared store.
    /// Working/short-term data is intentionally not written here.
    pub fn save_long_term(&self, profile_key: &str, memory: &AgentMemory) -> Result<(), AppError> {
        let facts: std::collections::BTreeMap<String, String> = memory
            .facts_in_layer(MemoryLayer::LongTerm)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let path = self.long_term_path(profile_key);
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            config_error(
                self.sessions_dir(),
                format!("could not create directory: {error}"),
            )
        })?;
        let temp_path = path.with_extension("longterm.toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(&LongTermMemory { facts })?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            config_error(
                path.clone(),
                format!("could not replace long-term memory file: {error}"),
            )
        })?;
        Ok(())
    }

    /// Load the profile-shared long-term facts (empty if none stored yet).
    pub fn load_long_term(&self, profile_key: &str) -> Result<LongTermMemory, AppError> {
        let path = self.long_term_path(profile_key);
        if !path.exists() {
            return Ok(LongTermMemory::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?;
        crate::toon_codec::from_str_or_json::<LongTermMemory>(&raw)
    }

    /// Seed `memory` with the profile's long-term facts. Existing keys in
    /// `memory` win; seeded keys are tagged [`MemoryLayer::LongTerm`].
    pub fn seed_long_term(
        &self,
        profile_key: &str,
        memory: &mut AgentMemory,
    ) -> Result<(), AppError> {
        let stored = self.load_long_term(profile_key)?;
        for (key, value) in stored.facts {
            memory.facts.entry(key.clone()).or_insert(value);
            memory.fact_layers.insert(key, MemoryLayer::LongTerm);
        }
        Ok(())
    }

    fn long_term_path(&self, profile_key: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("longterm-{}.memory.toon", safe_key(profile_key)))
    }

    // ----- Named persistent agents -------------------------------------------

    /// List saved agents (most recently updated first). Empty if none yet.
    pub fn list_agents(&self) -> Result<Vec<AgentSummary>, AppError> {
        let index: AgentsIndex = self
            .read_toon(&self.agents_index_path())?
            .unwrap_or_default();
        let mut agents = index.agents;
        agents.sort_by(|left, right| right.updated_at_unix.cmp(&left.updated_at_unix));
        Ok(agents)
    }

    pub fn load_agent(&self, id: &str) -> Result<Option<SavedAgent>, AppError> {
        self.read_toon(&self.agent_path(id))
    }

    /// Persist an agent's settings and upsert it into the index.
    pub fn save_agent(&self, agent: &SavedAgent) -> Result<(), AppError> {
        if agent.id.trim().is_empty() {
            return Err(AppError::InvalidInput("Agent id is required".to_string()));
        }
        self.write_toon(&self.agent_path(&agent.id), agent)?;
        let mut index: AgentsIndex = self
            .read_toon(&self.agents_index_path())?
            .unwrap_or_default();
        let summary = AgentSummary {
            id: agent.id.clone(),
            name: agent.name.clone(),
            provider: agent.provider.clone(),
            model: agent.model.clone(),
            stage: self.load_task(&agent.id)?.stage,
            updated_at_unix: agent.updated_at_unix,
        };
        if let Some(existing) = index.agents.iter_mut().find(|entry| entry.id == agent.id) {
            *existing = summary;
        } else {
            index.agents.push(summary);
        }
        self.write_toon(&self.agents_index_path(), &index)
    }

    pub fn delete_agent(&self, id: &str) -> Result<(), AppError> {
        // Remove the agent's dialog chats (history/metrics/memory) first.
        for dialog in self.list_dialogs(id)? {
            let _ = self.delete_dialog(id, &dialog.id);
        }
        for path in [
            self.agent_path(id),
            self.agent_task_path(id),
            self.agent_profile_path(id),
            self.agent_dialogs_path(id),
        ] {
            if path.exists() {
                fs::remove_file(&path)
                    .map_err(|error| config_error(path, format!("delete failed: {error}")))?;
            }
        }
        let mut index: AgentsIndex = self
            .read_toon(&self.agents_index_path())?
            .unwrap_or_default();
        index.agents.retain(|entry| entry.id != id);
        self.write_toon(&self.agents_index_path(), &index)
    }

    pub fn load_task(&self, id: &str) -> Result<TaskContext, AppError> {
        Ok(self
            .read_toon(&self.agent_task_path(id))?
            .unwrap_or_default())
    }

    pub fn load_dialog_task(
        &self,
        agent_id: &str,
        session_id: &str,
    ) -> Result<TaskContext, AppError> {
        let task_id = self.dialog_task_id(agent_id, session_id)?;
        self.load_scoped_task(agent_id, &task_id)
    }

    /// Persist the working-memory task and keep the picker index's stage in sync.
    pub fn save_task(&self, id: &str, task: &TaskContext) -> Result<(), AppError> {
        self.write_toon(&self.agent_task_path(id), task)?;
        let mut index: AgentsIndex = self
            .read_toon(&self.agents_index_path())?
            .unwrap_or_default();
        if let Some(entry) = index.agents.iter_mut().find(|entry| entry.id == id) {
            entry.stage = task.stage;
            self.write_toon(&self.agents_index_path(), &index)?;
        }
        Ok(())
    }

    pub fn save_dialog_task(
        &self,
        agent_id: &str,
        session_id: &str,
        task: &TaskContext,
    ) -> Result<(), AppError> {
        let task_id = self.dialog_task_id(agent_id, session_id)?;
        self.save_scoped_task(agent_id, &task_id, task)
    }

    pub fn load_scoped_task(&self, agent_id: &str, task_id: &str) -> Result<TaskContext, AppError> {
        Ok(self
            .read_toon(&self.agent_scoped_task_path(agent_id, task_id))?
            .unwrap_or_default())
    }

    pub fn save_scoped_task(
        &self,
        agent_id: &str,
        task_id: &str,
        task: &TaskContext,
    ) -> Result<(), AppError> {
        self.write_toon(&self.agent_scoped_task_path(agent_id, task_id), task)
    }

    pub fn dialog_task_id(&self, agent_id: &str, session_id: &str) -> Result<String, AppError> {
        let index: DialogsIndex = self
            .read_toon(&self.agent_dialogs_path(agent_id))?
            .unwrap_or_default();
        Ok(index
            .dialogs
            .iter()
            .find(|dialog| dialog.id == session_id)
            .map(|dialog| dialog.task_id.trim())
            .filter(|task_id| !task_id.is_empty())
            .unwrap_or("default")
            .to_string())
    }

    pub fn assign_dialog_task(
        &self,
        agent_id: &str,
        session_id: &str,
        task_id: &str,
    ) -> Result<(), AppError> {
        let path = self.agent_dialogs_path(agent_id);
        let mut index: DialogsIndex = self.read_toon(&path)?.unwrap_or_default();
        let task_id = task_id.trim();
        let task_id = if task_id.is_empty() {
            "default"
        } else {
            task_id
        };
        let now = unix_now();
        if let Some(entry) = index.dialogs.iter_mut().find(|d| d.id == session_id) {
            entry.task_id = task_id.to_string();
            entry.updated_at_unix = now;
        } else {
            index.dialogs.push(DialogMeta {
                id: session_id.to_string(),
                task_id: task_id.to_string(),
                title: String::new(),
                created_at_unix: now,
                updated_at_unix: now,
            });
        }
        self.write_toon(&path, &index)
    }

    pub fn load_profile(&self, id: &str) -> Result<AgentProfile, AppError> {
        Ok(self
            .read_toon(&self.agent_profile_path(id))?
            .unwrap_or_default())
    }

    pub fn save_profile(&self, id: &str, profile: &AgentProfile) -> Result<(), AppError> {
        self.write_toon(&self.agent_profile_path(id), profile)
    }

    pub fn list_user_profiles(&self) -> Result<Vec<UserProfile>, AppError> {
        let index: UserProfilesIndex = self
            .read_toon(&self.user_profiles_index_path())?
            .unwrap_or_default();
        let mut profiles = index.profiles;
        profiles.sort_by(|left, right| right.updated_at_unix.cmp(&left.updated_at_unix));
        Ok(profiles)
    }

    pub fn load_user_profile(&self, id: &str) -> Result<Option<UserProfile>, AppError> {
        let id = id.trim();
        if id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .list_user_profiles()?
            .into_iter()
            .find(|profile| profile.id == id))
    }

    pub fn save_user_profile(&self, profile: &UserProfile) -> Result<(), AppError> {
        if profile.id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "User profile id is required".to_string(),
            ));
        }
        let mut saved = profile.clone();
        saved.id = saved.id.trim().to_string();
        saved.display_name = saved.display_name.trim().to_string();
        if saved.display_name.is_empty() {
            saved.display_name = saved.id.clone();
        }
        let mut index: UserProfilesIndex = self
            .read_toon(&self.user_profiles_index_path())?
            .unwrap_or_default();
        if let Some(existing) = index.profiles.iter_mut().find(|entry| entry.id == saved.id) {
            *existing = saved;
        } else {
            index.profiles.push(saved);
        }
        self.write_toon(&self.user_profiles_index_path(), &index)
    }

    pub fn delete_user_profile(&self, id: &str) -> Result<(), AppError> {
        let id = id.trim();
        let mut index: UserProfilesIndex = self
            .read_toon(&self.user_profiles_index_path())?
            .unwrap_or_default();
        index.profiles.retain(|entry| entry.id != id);
        self.write_toon(&self.user_profiles_index_path(), &index)?;
        let mut bindings = self.load_user_profile_bindings()?;
        if bindings.active_profile_id == id {
            bindings.active_profile_id.clear();
        }
        bindings
            .default_profile_per_agent
            .retain(|_, profile_id| profile_id != id);
        self.save_user_profile_bindings(&bindings)
    }

    pub fn load_user_profile_bindings(&self) -> Result<UserProfileBindings, AppError> {
        Ok(self
            .read_toon(&self.user_profile_bindings_path())?
            .unwrap_or_default())
    }

    pub fn save_user_profile_bindings(
        &self,
        bindings: &UserProfileBindings,
    ) -> Result<(), AppError> {
        self.write_toon(&self.user_profile_bindings_path(), bindings)
    }

    pub fn resolve_user_profile(
        &self,
        explicit_profile_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Option<UserProfile>, AppError> {
        let bindings = self.load_user_profile_bindings()?;
        let selected = explicit_profile_id
            .and_then(blank_str_to_none)
            .map(str::to_string)
            .or_else(|| {
                agent_id
                    .and_then(blank_str_to_none)
                    .and_then(|id| bindings.default_profile_per_agent.get(id).cloned())
            })
            .or_else(|| blank_str_to_none(&bindings.active_profile_id).map(str::to_string));
        match selected {
            Some(id) => self.load_user_profile(&id),
            None => Ok(None),
        }
    }

    /// All dialogs (chats) of an agent, newest first.
    pub fn list_dialogs(&self, agent_id: &str) -> Result<Vec<DialogMeta>, AppError> {
        let index: DialogsIndex = self
            .read_toon(&self.agent_dialogs_path(agent_id))?
            .unwrap_or_default();
        let mut dialogs = index.dialogs;
        dialogs.sort_by(|left, right| right.updated_at_unix.cmp(&left.updated_at_unix));
        Ok(dialogs)
    }

    /// Upsert a dialog into the agent's index and bump its `updated_at`. The title
    /// is only set when empty (first message), so later turns don't overwrite it.
    pub fn register_dialog(
        &self,
        agent_id: &str,
        session_id: &str,
        title: &str,
    ) -> Result<(), AppError> {
        let path = self.agent_dialogs_path(agent_id);
        let mut index: DialogsIndex = self.read_toon(&path)?.unwrap_or_default();
        let now = unix_now();
        let title = title.trim();
        if let Some(entry) = index.dialogs.iter_mut().find(|d| d.id == session_id) {
            entry.updated_at_unix = now;
            if entry.title.trim().is_empty() && !title.is_empty() {
                entry.title = title.to_string();
            }
        } else {
            index.dialogs.push(DialogMeta {
                id: session_id.to_string(),
                task_id: default_task_id(),
                title: title.to_string(),
                created_at_unix: now,
                updated_at_unix: now,
            });
        }
        self.write_toon(&path, &index)
    }

    pub fn rename_dialog(
        &self,
        agent_id: &str,
        session_id: &str,
        title: &str,
    ) -> Result<(), AppError> {
        let path = self.agent_dialogs_path(agent_id);
        let mut index: DialogsIndex = self.read_toon(&path)?.unwrap_or_default();
        if let Some(entry) = index.dialogs.iter_mut().find(|d| d.id == session_id) {
            entry.title = title.trim().to_string();
            entry.updated_at_unix = unix_now();
        }
        self.write_toon(&path, &index)
    }

    /// Remove a dialog: its session/metrics/memory files and its index entry.
    pub fn delete_dialog(&self, agent_id: &str, session_id: &str) -> Result<(), AppError> {
        for path in [
            self.session_path(session_id),
            self.metrics_path(session_id),
            self.memory_path(session_id),
        ] {
            if path.exists() {
                fs::remove_file(&path)
                    .map_err(|error| config_error(path, format!("delete failed: {error}")))?;
            }
        }
        let path = self.agent_dialogs_path(agent_id);
        let mut index: DialogsIndex = self.read_toon(&path)?.unwrap_or_default();
        index.dialogs.retain(|d| d.id != session_id);
        self.write_toon(&path, &index)
    }

    fn agent_dialogs_path(&self, agent_id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("agent-{}.dialogs.toon", safe_key(agent_id)))
    }

    fn agent_path(&self, id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("agent-{}.agent.toon", safe_key(id)))
    }

    fn agent_task_path(&self, id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("agent-{}.task.toon", safe_key(id)))
    }

    fn agent_scoped_task_path(&self, agent_id: &str, task_id: &str) -> PathBuf {
        self.sessions_dir().join(format!(
            "agent-{}.task-{}.toon",
            safe_key(agent_id),
            safe_key(task_id)
        ))
    }

    fn agent_profile_path(&self, id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("agent-{}.profile.toon", safe_key(id)))
    }

    fn agents_index_path(&self) -> PathBuf {
        self.sessions_dir().join("agents-index.toon")
    }

    fn user_profiles_index_path(&self) -> PathBuf {
        self.sessions_dir().join("user-profiles-index.toon")
    }

    fn user_profile_bindings_path(&self) -> PathBuf {
        self.sessions_dir().join("user-profile-bindings.toon")
    }

    /// Atomic TOON write (temp file + rename), creating the data dir as needed.
    fn write_toon<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<(), AppError> {
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            config_error(
                self.sessions_dir(),
                format!("could not create directory: {error}"),
            )
        })?;
        let temp_path = path.with_extension("toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(value)?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, path)
            .map_err(|error| config_error(path, format!("could not replace file: {error}")))
    }

    /// Read a TOON (with JSON fallback) value, `None` when the file is absent.
    fn read_toon<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Result<Option<T>, AppError> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path)
            .map_err(|error| config_error(path, format!("read failed: {error}")))?;
        crate::toon_codec::from_str_or_json::<T>(&raw).map(Some)
    }

    pub fn topic_file_storage(&self, session_id: &str) -> Result<TopicFileStorage, AppError> {
        validate_session_id(session_id)?;
        Ok(TopicFileStorage {
            root: self.sessions_dir(),
            session_id: session_id.to_string(),
        })
    }

    pub fn save_metrics(&self, session_id: &str, metrics: &RequestMetrics) -> Result<(), AppError> {
        validate_session_id(session_id)?;
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            config_error(
                self.sessions_dir(),
                format!("could not create directory: {error}"),
            )
        })?;

        let path = self.metrics_path(session_id);
        let temp_path = path.with_extension("metrics.toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(&stored_metrics_from_request(metrics))?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            config_error(
                path.clone(),
                format!("could not replace metrics file: {error}"),
            )
        })?;
        Ok(())
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn index_dir(&self) -> PathBuf {
        self.root.join("index")
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{session_id}.toon"))
    }

    fn memory_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("{session_id}.memory.toon"))
    }

    fn metrics_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("{session_id}.metrics.toon"))
    }

    fn legacy_metrics_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("{session_id}.metrics.json"))
    }

    fn legacy_session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{session_id}.jsonl"))
    }

    fn load_legacy_jsonl_session(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<ConversationSession, AppError> {
        let file = fs::File::open(path)
            .map_err(|error| config_error(path, format!("open failed: {error}")))?;
        let mut messages = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|error| {
                config_error(path, format!("could not read session line: {error}"))
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let message = serde_json::from_str::<ChatMessage>(&line)
                .map_err(|error| AppError::Json(error.to_string()))?;
            messages.push(message);
        }
        Ok(ConversationSession {
            id: session_id.to_string(),
            messages,
            metrics: self.load_metrics(session_id)?,
        })
    }

    fn load_legacy_json_metrics(&self, path: &Path) -> Result<RequestMetrics, AppError> {
        let raw = fs::read_to_string(path)
            .map_err(|error| config_error(path, format!("read failed: {error}")))?;
        serde_json::from_str::<RequestMetrics>(&raw)
            .map_err(|error| AppError::Json(error.to_string()))
    }

    fn index_path(&self, profile_key: &str) -> PathBuf {
        self.index_dir()
            .join(format!("{}.txt", safe_key(profile_key)))
    }
}

impl TopicFileStorage {
    pub fn load_topic_file(&self, topic_id: &str) -> Result<Option<TopicFile>, AppError> {
        validate_session_id(&self.session_id)?;
        let path = self.topic_path(topic_id);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)
            .map_err(|error| config_error(path.clone(), format!("read failed: {error}")))?;
        crate::toon_codec::from_str_or_json::<TopicFile>(&raw).map(Some)
    }

    pub fn save_topic_file(&self, topic_file: &TopicFile) -> Result<(), AppError> {
        validate_session_id(&self.session_id)?;
        fs::create_dir_all(self.topic_dir()).map_err(|error| {
            config_error(
                self.topic_dir(),
                format!("could not create directory: {error}"),
            )
        })?;
        let path = self.topic_path(&topic_file.metadata.id);
        let temp_path = path.with_extension("topic.toon.tmp");
        {
            let mut file = fs::File::create(&temp_path).map_err(|error| {
                config_error(temp_path.clone(), format!("create failed: {error}"))
            })?;
            let raw = crate::toon_codec::to_string(topic_file)?;
            writeln!(file, "{raw}").map_err(|error| {
                config_error(temp_path.clone(), format!("write failed: {error}"))
            })?;
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            config_error(
                path.clone(),
                format!("could not replace topic file: {error}"),
            )
        })?;
        Ok(())
    }

    fn topic_dir(&self) -> PathBuf {
        self.root.join(format!("{}.topics", self.session_id))
    }

    fn topic_path(&self, topic_id: &str) -> PathBuf {
        self.topic_dir()
            .join(format!("{}.topic.toon", safe_key(topic_id)))
    }
}

fn stored_messages_from_chat(messages: &[ChatMessage]) -> Vec<StoredChatMessage> {
    messages
        .iter()
        .map(|message| StoredChatMessage {
            role: match &message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            }
            .to_string(),
            content: message.content.clone(),
        })
        .collect()
}

fn stored_messages_into_chat(
    messages: Vec<StoredChatMessage>,
) -> Result<Vec<ChatMessage>, AppError> {
    messages
        .into_iter()
        .map(|message| {
            Ok(ChatMessage {
                role: parse_role(&message.role)?,
                content: message.content,
            })
        })
        .collect()
}

fn stored_metrics_from_request(metrics: &RequestMetrics) -> StoredRequestMetrics {
    StoredRequestMetrics {
        elapsed_ms: metrics.elapsed_ms,
        usage: metrics.usage.clone(),
        cost: metrics.cost.as_ref().map(|cost| StoredRequestCost {
            amount: cost.amount,
            currency: cost.currency.clone(),
            source: cost.source.to_string(),
        }),
    }
}

fn stored_metrics_into_request(metrics: StoredRequestMetrics) -> Result<RequestMetrics, AppError> {
    Ok(RequestMetrics {
        elapsed_ms: metrics.elapsed_ms,
        usage: metrics.usage,
        cost: metrics
            .cost
            .map(|cost| {
                Ok::<RequestCost, AppError>(RequestCost {
                    amount: cost.amount,
                    currency: cost.currency,
                    source: parse_cost_source(&cost.source)?,
                })
            })
            .transpose()?,
    })
}

fn parse_cost_source(value: &str) -> Result<crate::providers::CostSource, AppError> {
    match value {
        "provider-reported" => Ok(crate::providers::CostSource::ProviderReported),
        "configured-pricing" => Ok(crate::providers::CostSource::ConfiguredPricing),
        "billing-api" => Ok(crate::providers::CostSource::BillingApi),
        other => Err(AppError::Json(format!("unsupported cost source: {other}"))),
    }
}

pub fn add_request_metrics(total: &RequestMetrics, request: &RequestMetrics) -> RequestMetrics {
    RequestMetrics {
        elapsed_ms: total.elapsed_ms.saturating_add(request.elapsed_ms),
        usage: add_token_usage(total.usage.as_ref(), request.usage.as_ref()),
        cost: add_request_cost(total.cost.as_ref(), request.cost.as_ref()),
    }
}

fn add_token_usage(total: Option<&TokenUsage>, request: Option<&TokenUsage>) -> Option<TokenUsage> {
    match (total, request) {
        (Some(total), Some(request)) => Some(TokenUsage {
            input_tokens: total.input_tokens.saturating_add(request.input_tokens),
            output_tokens: total.output_tokens.saturating_add(request.output_tokens),
            total_tokens: total.total_tokens.saturating_add(request.total_tokens),
            cache_hit_input_tokens: add_optional_u32(
                total.cache_hit_input_tokens,
                request.cache_hit_input_tokens,
            ),
            cache_miss_input_tokens: add_optional_u32(
                total.cache_miss_input_tokens,
                request.cache_miss_input_tokens,
            ),
            input_audio_tokens: add_optional_u32(
                total.input_audio_tokens,
                request.input_audio_tokens,
            ),
            output_reasoning_tokens: add_optional_u32(
                total.output_reasoning_tokens,
                request.output_reasoning_tokens,
            ),
            output_visible_tokens: add_optional_u32(
                total.output_visible_tokens,
                request.output_visible_tokens,
            ),
            output_audio_tokens: add_optional_u32(
                total.output_audio_tokens,
                request.output_audio_tokens,
            ),
            accepted_prediction_output_tokens: add_optional_u32(
                total.accepted_prediction_output_tokens,
                request.accepted_prediction_output_tokens,
            ),
            rejected_prediction_output_tokens: add_optional_u32(
                total.rejected_prediction_output_tokens,
                request.rejected_prediction_output_tokens,
            ),
        }),
        (Some(total), None) => Some(total.clone()),
        (None, Some(request)) => Some(request.clone()),
        (None, None) => None,
    }
}

fn add_optional_u32(total: Option<u32>, request: Option<u32>) -> Option<u32> {
    match (total, request) {
        (Some(total), Some(request)) => Some(total.saturating_add(request)),
        (Some(total), None) => Some(total),
        (None, Some(request)) => Some(request),
        (None, None) => None,
    }
}

fn add_request_cost(
    total: Option<&RequestCost>,
    request: Option<&RequestCost>,
) -> Option<RequestCost> {
    match (total, request) {
        (Some(total), Some(request))
            if total.currency == request.currency && total.source == request.source =>
        {
            Some(RequestCost {
                amount: total.amount + request.amount,
                currency: total.currency.clone(),
                source: total.source.clone(),
            })
        }
        (Some(total), None) => Some(total.clone()),
        (None, Some(request)) => Some(request.clone()),
        _ => None,
    }
}

fn parse_role(value: &str) -> Result<Role, AppError> {
    match value {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        other => Err(AppError::Json(format!("unsupported chat role: {other}"))),
    }
}

pub fn session_key(profile_name: &str, model: &str) -> String {
    format!("{profile_name}:{model}")
}

pub fn web_session_key(agent_id: &str, provider: &str, model: &str) -> String {
    format!("web:{agent_id}:{provider}:{model}")
}

fn safe_key(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || *byte == b'-' {
            out.push(char::from(*byte));
        } else if *byte == b'_' {
            out.push_str("__");
        } else {
            out.push_str(&format!("_{byte:02x}"));
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

fn validate_session_id(session_id: &str) -> Result<(), AppError> {
    if !session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Ok(());
    }
    Err(AppError::InvalidInput(
        "Session id contains unsupported characters".to_string(),
    ))
}

fn config_error(path: impl AsRef<Path>, message: String) -> AppError {
    AppError::Config {
        path: path.as_ref().to_path_buf(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CostSource, Role};

    #[test]
    fn local_session_store_roundtrips_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let session = store.create_session().expect("create session");
        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: "hello".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "hi".to_string(),
            },
        ];

        store
            .save_session("profile:model", &session.id, &messages)
            .expect("save session");
        let loaded = store
            .load_or_create_latest("profile:model")
            .expect("load latest");

        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.metrics, RequestMetrics::default());
    }

    #[test]
    fn long_term_survives_new_session_short_term_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let profile = "profile:model";

        // Session 1: a long-term fact + short-term summary.
        let mut memory = AgentMemory::default();
        memory.set_fact_in_layer(
            "preferences".to_string(),
            "concise answers".to_string(),
            MemoryLayer::LongTerm,
        );
        memory.set_fact_in_layer(
            "goal".to_string(),
            "ship feature".to_string(),
            MemoryLayer::Working,
        );
        memory.session_summary = Some("earlier discussion".to_string());
        store
            .save_long_term(profile, &memory)
            .expect("save long-term");

        // Session 2: brand-new memory, only long-term seeded.
        let mut fresh = AgentMemory::default();
        store.seed_long_term(profile, &mut fresh).expect("seed");

        assert_eq!(
            fresh.facts.get("preferences").map(String::as_str),
            Some("concise answers"),
            "long-term fact must carry into a new session"
        );
        assert_eq!(fresh.fact_layer("preferences"), MemoryLayer::LongTerm);
        assert!(
            !fresh.facts.contains_key("goal"),
            "working-layer fact must not leak into a new session"
        );
        assert!(
            fresh.session_summary.is_none(),
            "short-term summary must be empty in a new session"
        );
    }

    #[test]
    fn agent_long_term_facts_are_shared_between_dialogs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let agent_key = "agent:menu-agent";

        let mut first_dialog_memory = AgentMemory::default();
        first_dialog_memory.set_fact_in_layer(
            "available_products".to_string(),
            "tomatoes, basil, mozzarella".to_string(),
            MemoryLayer::LongTerm,
        );
        first_dialog_memory.set_fact_in_layer(
            "current_task".to_string(),
            "invent menu".to_string(),
            MemoryLayer::Working,
        );
        store
            .save_long_term(agent_key, &first_dialog_memory)
            .expect("save agent long-term");

        let mut second_dialog_memory = AgentMemory::default();
        store
            .seed_long_term(agent_key, &mut second_dialog_memory)
            .expect("seed agent long-term");

        assert_eq!(
            second_dialog_memory
                .facts
                .get("available_products")
                .map(String::as_str),
            Some("tomatoes, basil, mozzarella"),
            "manually saved long-term facts should be visible in another dialog"
        );
        assert_eq!(
            second_dialog_memory.fact_layer("available_products"),
            MemoryLayer::LongTerm
        );
        assert!(
            !second_dialog_memory.facts.contains_key("current_task"),
            "working facts should not leak between dialogs"
        );
    }

    #[test]
    fn saved_agents_roundtrip_list_and_delete() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));

        let agent = SavedAgent {
            id: "a1".to_string(),
            name: "Rust reviewer".to_string(),
            provider: "openai-compatible".to_string(),
            model: "gpt-4.1-mini".to_string(),
            system_prompt: "be terse".to_string(),
            updated_at_unix: 100,
            ..SavedAgent::default()
        };
        store.save_agent(&agent).expect("save agent");

        // Working (task + stage) + long-term (profile) layers.
        let task = TaskContext {
            stage: TaskStage::Planning,
            title: "ship agents".to_string(),
            goal: "persist settings".to_string(),
            plan: vec!["design".to_string(), "build".to_string()],
            ..TaskContext::default()
        };
        store.save_task("a1", &task).expect("save task");
        store
            .save_profile(
                "a1",
                &AgentProfile {
                    fields: vec![ProfileField {
                        key: "stack".to_string(),
                        question: "Which stack?".to_string(),
                        required: true,
                        value: "Rust".to_string(),
                    }],
                    updated_at_unix: 100,
                },
            )
            .expect("save profile");

        let loaded = store.load_agent("a1").expect("load").expect("present");
        assert_eq!(loaded.name, "Rust reviewer");
        assert_eq!(loaded.system_prompt, "be terse");
        let loaded_task = store.load_task("a1").expect("task");
        assert_eq!(loaded_task.title, "ship agents");
        assert_eq!(loaded_task.stage, TaskStage::Planning);
        let loaded_profile = store.load_profile("a1").expect("profile");
        assert_eq!(loaded_profile.fields[0].value, "Rust");
        assert!(loaded_profile.pending_required().is_empty());

        let listed = store.list_agents().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "a1");
        // Index stage stays in sync with the saved task.
        assert_eq!(listed[0].stage, TaskStage::Planning);

        store.delete_agent("a1").expect("delete");
        assert!(store.load_agent("a1").expect("load after delete").is_none());
        assert!(store.list_agents().expect("list after delete").is_empty());
    }

    #[test]
    fn user_profiles_are_reusable_runtime_bindings_not_agent_owned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        store
            .save_agent(&SavedAgent {
                id: "coding-agent".to_string(),
                name: "Coding".to_string(),
                updated_at_unix: 1,
                ..SavedAgent::default()
            })
            .expect("save coding agent");
        store
            .save_agent(&SavedAgent {
                id: "research-agent".to_string(),
                name: "Research".to_string(),
                updated_at_unix: 2,
                ..SavedAgent::default()
            })
            .expect("save research agent");

        let profile = UserProfile {
            id: "artem-short-russian".to_string(),
            display_name: "Artem short Russian".to_string(),
            style_preferences: vec!["short".to_string()],
            language_preferences: vec!["Russian".to_string()],
            updated_at_unix: 10,
            ..UserProfile::default()
        };
        store.save_user_profile(&profile).expect("save profile");
        let mut bindings = UserProfileBindings {
            active_profile_id: profile.id.clone(),
            ..UserProfileBindings::default()
        };
        bindings
            .default_profile_per_agent
            .insert("research-agent".to_string(), profile.id.clone());
        store
            .save_user_profile_bindings(&bindings)
            .expect("save bindings");

        let coding_profile = store
            .resolve_user_profile(None, Some("coding-agent"))
            .expect("resolve coding")
            .expect("coding profile");
        let research_profile = store
            .resolve_user_profile(None, Some("research-agent"))
            .expect("resolve research")
            .expect("research profile");
        assert_eq!(coding_profile.id, profile.id);
        assert_eq!(research_profile.id, profile.id);

        store.delete_agent("research-agent").expect("delete agent");
        assert!(
            store
                .load_user_profile("artem-short-russian")
                .expect("load profile")
                .is_some(),
            "deleting an agent must not delete a reusable user profile"
        );
        assert!(
            store
                .load_agent("coding-agent")
                .expect("load coding")
                .expect("coding present")
                .system_prompt
                .is_empty(),
            "agent definition must not duplicate user profile preferences"
        );
    }

    #[test]
    fn user_profile_resolution_priority_is_explicit_agent_default_then_active() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        for id in ["active", "agent-default", "explicit"] {
            store
                .save_user_profile(&UserProfile {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    updated_at_unix: 1,
                    ..UserProfile::default()
                })
                .expect("save profile");
        }
        let mut bindings = UserProfileBindings {
            active_profile_id: "active".to_string(),
            ..UserProfileBindings::default()
        };
        bindings
            .default_profile_per_agent
            .insert("coding-agent".to_string(), "agent-default".to_string());
        store
            .save_user_profile_bindings(&bindings)
            .expect("save bindings");

        assert_eq!(
            store
                .resolve_user_profile(Some("explicit"), Some("coding-agent"))
                .expect("explicit")
                .expect("profile")
                .id,
            "explicit",
            "explicit runtime selection must win"
        );
        assert_eq!(
            store
                .resolve_user_profile(None, Some("coding-agent"))
                .expect("agent default")
                .expect("profile")
                .id,
            "agent-default",
            "agent default is selection metadata, not ownership"
        );
        assert_eq!(
            store
                .resolve_user_profile(None, Some("other-agent"))
                .expect("active")
                .expect("profile")
                .id,
            "active",
            "active profile is the fallback"
        );
        assert!(
            store
                .resolve_user_profile(Some("missing"), Some("coding-agent"))
                .expect("missing explicit")
                .is_none(),
            "a missing explicit profile must not silently fall back to another profile"
        );
    }

    #[test]
    fn task_stage_transitions_enforced() {
        // Legal forward transitions.
        assert!(TaskStage::Clarify.can_transition(TaskStage::Planning));
        assert!(TaskStage::Planning.can_transition(TaskStage::Execution));
        assert!(TaskStage::Execution.can_transition(TaskStage::Validation));
        assert!(TaskStage::Validation.can_transition(TaskStage::Done));
        // Legal back-transitions.
        assert!(TaskStage::Execution.can_transition(TaskStage::Planning));
        assert!(TaskStage::Validation.can_transition(TaskStage::Execution));
        // Staying put is allowed.
        assert!(TaskStage::Planning.can_transition(TaskStage::Planning));
        // Illegal jumps are rejected.
        assert!(!TaskStage::Clarify.can_transition(TaskStage::Execution));
        assert!(!TaskStage::Planning.can_transition(TaskStage::Done));
        assert!(!TaskStage::Done.can_transition(TaskStage::Planning));
        // Round-trips through its string form.
        assert_eq!("execution".parse(), Ok(TaskStage::Execution));
        assert_eq!(TaskStage::Validation.to_string(), "validation");
    }

    #[test]
    fn agent_dialogs_roundtrip_and_auto_register() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));

        assert_eq!(agent_id_from_key("agent:a1"), Some("a1"));
        assert_eq!(agent_id_from_key("web:x:y:z"), None);

        // save_session under an agent key auto-registers a dialog with a title
        // derived from the first user message.
        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: "Спланируй экран логина для приложения".to_string(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "ок".to_string(),
            },
        ];
        store
            .save_session("agent:a1", "sess-1", &messages)
            .expect("save session");
        store
            .save_session("agent:a1", "sess-2", &[])
            .expect("save empty session");
        let task_1 = TaskContext {
            title: "menu".to_string(),
            goal: "invent dishes".to_string(),
            ..TaskContext::default()
        };
        let task_2 = TaskContext {
            title: "slogan".to_string(),
            goal: "invent slogan".to_string(),
            ..TaskContext::default()
        };
        store
            .save_dialog_task("a1", "sess-1", &task_1)
            .expect("save shared default task");
        assert_eq!(
            store
                .load_dialog_task("a1", "sess-1")
                .expect("load dialog task 1")
                .goal,
            "invent dishes"
        );
        assert_eq!(
            store
                .load_dialog_task("a1", "sess-2")
                .expect("load shared task from dialog 2")
                .goal,
            "invent dishes"
        );
        store
            .assign_dialog_task("a1", "sess-2", "slogan")
            .expect("assign dialog 2 to another task");
        store
            .save_dialog_task("a1", "sess-2", &task_2)
            .expect("save dialog task 2");
        assert_eq!(store.dialog_task_id("a1", "sess-1").unwrap(), "default");
        assert_eq!(store.dialog_task_id("a1", "sess-2").unwrap(), "slogan");
        assert_eq!(
            store.load_dialog_task("a1", "sess-1").unwrap().goal,
            "invent dishes"
        );
        assert_eq!(
            store.load_dialog_task("a1", "sess-2").unwrap().goal,
            "invent slogan"
        );

        let dialogs = store.list_dialogs("a1").expect("list");
        assert_eq!(dialogs.len(), 2);
        let first = dialogs.iter().find(|d| d.id == "sess-1").expect("sess-1");
        assert!(first.title.starts_with("Спланируй экран логина"));

        store
            .rename_dialog("a1", "sess-1", "Логин")
            .expect("rename");
        assert_eq!(
            store
                .list_dialogs("a1")
                .unwrap()
                .iter()
                .find(|d| d.id == "sess-1")
                .unwrap()
                .title,
            "Логин"
        );

        store.delete_dialog("a1", "sess-2").expect("delete");
        let after = store.list_dialogs("a1").expect("list after delete");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, "sess-1");
        assert_eq!(
            store.load_scoped_task("a1", "slogan").unwrap().goal,
            "invent slogan",
            "deleting one dialog must not delete its task scope"
        );
        // Ad-hoc (non-agent) sessions are not registered as dialogs.
        store
            .save_session("web:local:openai:gpt", "sess-3", &messages)
            .expect("save adhoc");
        assert!(store.list_dialogs("other").unwrap().is_empty());
    }

    #[test]
    fn local_session_store_roundtrips_metrics_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let session = store.create_session().expect("create session");
        let metrics = RequestMetrics {
            elapsed_ms: 123,
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                cache_hit_input_tokens: Some(4),
                cache_miss_input_tokens: Some(6),
                ..TokenUsage::default()
            }),
            cost: Some(RequestCost {
                amount: 0.00042,
                currency: "USD".to_string(),
                source: CostSource::ConfiguredPricing,
            }),
        };

        store
            .save_metrics(&session.id, &metrics)
            .expect("save metrics");
        let loaded = store.load_metrics(&session.id).expect("load metrics");

        assert_eq!(loaded, metrics);
    }

    #[test]
    fn request_metrics_accumulate_usage_and_matching_costs() {
        let total = RequestMetrics {
            elapsed_ms: 100,
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                cache_hit_input_tokens: Some(3),
                cache_miss_input_tokens: None,
                output_reasoning_tokens: Some(4),
                output_visible_tokens: Some(1),
                ..TokenUsage::default()
            }),
            cost: Some(RequestCost {
                amount: 0.001,
                currency: "USD".to_string(),
                source: CostSource::ConfiguredPricing,
            }),
        };
        let request = RequestMetrics {
            elapsed_ms: 200,
            usage: Some(TokenUsage {
                input_tokens: 7,
                output_tokens: 11,
                total_tokens: 18,
                cache_hit_input_tokens: Some(2),
                cache_miss_input_tokens: Some(5),
                output_reasoning_tokens: Some(6),
                output_visible_tokens: Some(5),
                ..TokenUsage::default()
            }),
            cost: Some(RequestCost {
                amount: 0.002,
                currency: "USD".to_string(),
                source: CostSource::ConfiguredPricing,
            }),
        };

        let added = add_request_metrics(&total, &request);

        assert_eq!(added.elapsed_ms, 300);
        assert_eq!(
            added.usage,
            Some(TokenUsage {
                input_tokens: 17,
                output_tokens: 16,
                total_tokens: 33,
                cache_hit_input_tokens: Some(5),
                cache_miss_input_tokens: Some(5),
                output_reasoning_tokens: Some(10),
                output_visible_tokens: Some(6),
                ..TokenUsage::default()
            })
        );
        assert_eq!(
            added.cost,
            Some(RequestCost {
                amount: 0.003,
                currency: "USD".to_string(),
                source: CostSource::ConfiguredPricing,
            })
        );
    }

    #[test]
    fn local_session_store_roundtrips_memory_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let session = store.create_session().expect("create session");
        let memory = AgentMemory {
            facts: Default::default(),
            branch_assignments: Default::default(),
            session_summary: Some("User prefers short technical answers.".to_string()),
            summarized_message_count: 8,
            ..AgentMemory::default()
        };

        store
            .save_memory(&session.id, &memory)
            .expect("save memory");
        let loaded = store.load_memory(&session.id).expect("load memory");

        assert_eq!(loaded, memory);
    }

    #[test]
    fn local_session_store_roundtrips_topic_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let session = store.create_session().expect("create session");
        let topic_store = store.topic_file_storage(&session.id).expect("topic store");
        let topic = TopicFile {
            metadata: crate::chat::memory::TopicMetadata {
                id: "rust async".to_string(),
                title: "Rust async".to_string(),
                short_description: "Ownership and async context.".to_string(),
                tags: vec!["rust".to_string(), "async".to_string()],
                message_count: 4,
                updated_at_unix: 123,
            },
            context: "user: borrow checker\nassistant: use ownership boundaries".to_string(),
        };

        topic_store.save_topic_file(&topic).expect("save topic");
        let loaded = topic_store
            .load_topic_file("rust async")
            .expect("load topic")
            .expect("topic exists");

        assert_eq!(loaded, topic);
    }

    #[test]
    fn local_session_store_rejects_path_like_session_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));

        assert!(matches!(
            store.load_session("../secret"),
            Err(AppError::InvalidInput(_))
        ));
        assert!(matches!(
            store.load_session(""),
            Err(AppError::InvalidInput(_))
        ));
    }
}
