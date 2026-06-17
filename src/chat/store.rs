use std::{
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

pub use super::store_types::{
    AgentProfile, AgentSummary, DialogMeta, LongTermMemory, ProfileField, SavedAgent,
    SavedMemoryConfig, TaskArtifact, TaskContext, TaskPipelineStage, TaskStage, TaskWorkerAgent,
    UserProfile, UserProfileBindings, agent_id_from_key, unix_now,
};
use super::store_types::{
    AgentsIndex, DialogsIndex, UserProfilesIndex, blank_str_to_none, default_task_id,
    dialog_title_from_messages,
};

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
