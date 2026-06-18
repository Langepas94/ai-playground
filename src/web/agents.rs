use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::{
    chat::memory::{MemoryConfig, MemoryLayer},
    chat::{
        AgentMemory, AgentProfile, AgentSummary, LocalSessionStore, ProfileField, SavedAgent,
        SavedMemoryConfig, SwarmConfig, TaskArtifact, TaskContext, TaskPipelineStage, UserProfile,
        UserProfileBindings, build_profile_schema,
    },
    config::ProfileConfig,
    errors::AppError,
    providers::validate_base_url,
};

use super::{
    ApiJson, AppState, WebError, WebMemoryConfig, blank_str_to_none, blank_to_none, parse_provider,
    resolve_web_token,
};

pub(super) async fn agents_manage(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<AgentsManageRequest>,
) -> Result<Json<AgentsManageResponse>, WebError> {
    let mut response = AgentsManageResponse::default();
    match request.action.as_str() {
        "list" => {
            response.agents = state.sessions.list_agents()?;
        }
        "load" => {
            let id = require_agent_id(request.id.as_deref())?;
            let agent = state
                .sessions
                .load_agent(&id)?
                .ok_or_else(|| AppError::InvalidInput(format!("Unknown agent: {id}")))?;
            response.agent = Some(agent);
            response.task = match request.session_id.as_deref().and_then(blank_str_to_none) {
                Some(session_id) => {
                    response.task_id = state.sessions.dialog_task_id(&id, session_id)?;
                    Some(state.sessions.load_dialog_task(&id, session_id)?)
                }
                None => {
                    response.task_id = "default".to_string();
                    Some(state.sessions.load_task(&id)?)
                }
            };
            response.profile = Some(state.sessions.load_profile(&id)?);
            response.long_term_facts = load_agent_long_term_facts(&state.sessions, &id)?;
        }
        "save" => {
            let payload = request
                .agent
                .ok_or_else(|| AppError::InvalidInput("Agent payload is required".to_string()))?;
            if payload.name.trim().is_empty() {
                return Err(AppError::InvalidInput("Agent name is required".to_string()).into());
            }
            let id = match payload.id.as_deref().and_then(blank_str_to_none) {
                Some(id) => id.to_string(),
                None => uuid::Uuid::new_v4().to_string(),
            };
            let created_at_unix = state
                .sessions
                .load_agent(&id)?
                .map(|existing| existing.created_at_unix)
                .filter(|value| *value > 0)
                .unwrap_or_else(crate::chat::unix_now);
            let saved = SavedAgent {
                id: id.clone(),
                name: payload.name.trim().to_string(),
                provider: payload.provider.trim().to_string(),
                base_url: payload
                    .base_url
                    .clone()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                model: payload.model.trim().to_string(),
                system_prompt: payload.system_prompt.clone().unwrap_or_default(),
                domain: payload
                    .domain
                    .clone()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                invariants: clean_lines(payload.invariants.clone().unwrap_or_default()),
                memory: saved_memory_from_web(payload.memory.clone().unwrap_or_default()),
                swarm: crate::chat::SwarmConfig::defaults(),
                created_at_unix,
                updated_at_unix: crate::chat::unix_now(),
            };
            state.sessions.save_agent(&saved)?;
            persist_agent_initial_context(
                &state.sessions,
                &id,
                payload.initial_context.as_deref(),
            )?;
            // Persist the interview schema (long-term layer) if the UI sent one,
            // preserving any values already elicited for matching keys.
            // Schema source: explicit LLM-built schema if sent, otherwise the
            // raw "что уточнить" seed fields become required interview fields
            // deterministically (DEMO-003: seed list must create the schema).
            let schema = payload.profile_schema.clone().or_else(|| {
                let seeds = clean_lines(payload.seed_fields.clone().unwrap_or_default());
                if seeds.is_empty() {
                    None
                } else {
                    Some(
                        seeds
                            .into_iter()
                            .map(|key| ProfileFieldPayload {
                                key: key.clone(),
                                question: key,
                                required: true,
                            })
                            .collect(),
                    )
                }
            });
            if let Some(schema) = schema {
                let mut profile = state.sessions.load_profile(&id)?;
                profile.fields = merge_profile_schema(profile.fields, schema);
                profile.updated_at_unix = crate::chat::unix_now();
                state.sessions.save_profile(&id, &profile)?;
                response.profile = Some(profile);
            } else {
                response.profile = Some(state.sessions.load_profile(&id)?);
            }
            response.task = Some(state.sessions.load_task(&id)?);
            response.long_term_facts = load_agent_long_term_facts(&state.sessions, &id)?;
            response.agent = Some(saved);
        }
        "delete" => {
            let id = require_agent_id(request.id.as_deref())?;
            state.sessions.delete_agent(&id)?;
            response.agents = state.sessions.list_agents()?;
        }
        "build-schema" => {
            let payload = request
                .agent
                .ok_or_else(|| AppError::InvalidInput("Agent payload is required".to_string()))?;
            let project_info = payload.domain.clone().unwrap_or_default();
            if project_info.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "Project information is required to build a schema".to_string(),
                )
                .into());
            }
            let profile = payload_profile(&payload)?;
            validate_base_url(&profile.provider.to_string(), &profile.base_url)?;
            let token = resolve_web_token(
                state.secrets.as_ref(),
                &profile,
                request.token.as_deref().unwrap_or_default(),
                request.token_provider.as_deref(),
            )?;
            let seed = clean_lines(payload.seed_fields.clone().unwrap_or_default());
            let schema_domain = schema_domain_prompt(
                project_info.trim(),
                payload.initial_context.as_deref().unwrap_or_default(),
            );
            let fields = build_profile_schema(
                &state.client,
                &profile,
                &token,
                schema_domain.as_str(),
                &seed,
            )
            .await?;
            response.profile = Some(AgentProfile {
                fields,
                updated_at_unix: crate::chat::unix_now(),
            });
        }
        "dialogs" => {
            let id = require_agent_id(request.id.as_deref())?;
            response.dialogs = state.sessions.list_dialogs(&id)?;
        }
        "dialog-rename" => {
            let id = require_agent_id(request.id.as_deref())?;
            let session_id = request
                .session_id
                .as_deref()
                .and_then(blank_str_to_none)
                .ok_or_else(|| AppError::InvalidInput("session_id is required".to_string()))?;
            state.sessions.rename_dialog(
                &id,
                session_id,
                request.title.as_deref().unwrap_or_default(),
            )?;
            response.dialogs = state.sessions.list_dialogs(&id)?;
        }
        "dialog-delete" => {
            let id = require_agent_id(request.id.as_deref())?;
            let session_id = request
                .session_id
                .as_deref()
                .and_then(blank_str_to_none)
                .ok_or_else(|| AppError::InvalidInput("session_id is required".to_string()))?;
            state.sessions.delete_dialog(&id, session_id)?;
            response.dialogs = state.sessions.list_dialogs(&id)?;
        }
        "task-load" => {
            let id = require_agent_id(request.id.as_deref())?;
            let task_id = request
                .task_id
                .as_deref()
                .and_then(blank_str_to_none)
                .map(str::to_string)
                .unwrap_or_else(|| "default".to_string());
            if let Some(session_id) = request.session_id.as_deref().and_then(blank_str_to_none) {
                state
                    .sessions
                    .assign_dialog_task(&id, session_id, &task_id)?;
            }
            response.task_id = task_id.clone();
            response.task = Some(state.sessions.load_scoped_task(&id, &task_id)?);
        }
        "task-save" => {
            let id = require_agent_id(request.id.as_deref())?;
            let task_id = request
                .task_id
                .as_deref()
                .and_then(blank_str_to_none)
                .map(str::to_string)
                .unwrap_or_else(|| "default".to_string());
            let task = request
                .task
                .ok_or_else(|| AppError::InvalidInput("Task payload is required".to_string()))?
                .into_task_context();
            let saved_task = if let Some(session_id) =
                request.session_id.as_deref().and_then(blank_str_to_none)
            {
                state
                    .sessions
                    .assign_dialog_task(&id, session_id, &task_id)?;
                state.sessions.save_dialog_task(&id, session_id, &task)?;
                state.sessions.load_dialog_task(&id, session_id)?
            } else if task_id == "default" {
                state.sessions.save_task(&id, &task)?;
                state.sessions.load_task(&id)?
            } else {
                state.sessions.save_scoped_task(&id, &task_id, &task)?;
                state.sessions.load_scoped_task(&id, &task_id)?
            };
            response.task_id = task_id.clone();
            response.task = Some(saved_task);
        }
        "swarm-load" => {
            let id = require_agent_id(request.id.as_deref())?;
            let agent = state
                .sessions
                .load_agent(&id)?
                .ok_or_else(|| AppError::InvalidInput(format!("Unknown agent: {id}")))?;
            response.swarm = Some(agent.swarm.normalized());
        }
        "swarm-save" => {
            let id = require_agent_id(request.id.as_deref())?;
            let swarm = request
                .swarm
                .ok_or_else(|| AppError::InvalidInput("Swarm payload is required".to_string()))?
                .normalized();
            validate_swarm(&swarm)?;
            let mut agent = state
                .sessions
                .load_agent(&id)?
                .ok_or_else(|| AppError::InvalidInput(format!("Unknown agent: {id}")))?;
            agent.swarm = swarm;
            agent.updated_at_unix = crate::chat::unix_now();
            state.sessions.save_agent(&agent)?;
            response.swarm = Some(agent.swarm.clone());
            response.agent = Some(agent);
        }
        other => {
            return Err(AppError::InvalidInput(format!("Unknown agent action: {other}")).into());
        }
    }
    Ok(Json(response))
}

/// Validate a swarm config: any sub-agent overriding the provider must name a
/// known provider, and any explicit base_url must be well-formed.
fn validate_swarm(swarm: &SwarmConfig) -> Result<(), AppError> {
    for agent in &swarm.agents {
        let provider = agent.provider.trim();
        if provider.is_empty() {
            continue;
        }
        let kind = parse_provider(provider)?;
        let base_url = agent.base_url.trim();
        if !base_url.is_empty() {
            validate_base_url(&kind.to_string(), base_url)?;
        }
    }
    Ok(())
}

/// Build a `ProfileConfig` from an agent payload (for the schema-builder call).
fn payload_profile(payload: &AgentPayload) -> Result<ProfileConfig, AppError> {
    let provider = parse_provider(&payload.provider)?;
    if payload.model.trim().is_empty() {
        return Err(AppError::InvalidInput("Model is required".to_string()));
    }
    Ok(ProfileConfig {
        provider,
        model: payload.model.trim().to_string(),
        base_url: payload.base_url.clone().unwrap_or_default(),
        token_ref: String::new(),
    })
}

/// Trim, drop blanks, from a list of free-text lines (invariants / seed fields).
fn clean_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn schema_domain_prompt(domain: &str, initial_context: &str) -> String {
    let initial_context = initial_context.trim();
    if initial_context.is_empty() {
        domain.trim().to_string()
    } else {
        format!(
            "{}\n\nAlready known project context:\n{}",
            domain.trim(),
            initial_context
        )
    }
}

pub(super) fn persist_agent_initial_context(
    sessions: &LocalSessionStore,
    agent_id: &str,
    initial_context: Option<&str>,
) -> Result<(), AppError> {
    let Some(initial_context) = initial_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if crate::chat::memory::looks_sensitive(initial_context) {
        return Err(AppError::InvalidInput(
            "Initial agent context looks sensitive; do not save secrets in long-term memory"
                .to_string(),
        ));
    }

    let profile_key = format!("agent:{agent_id}");
    let mut memory = AgentMemory::default();
    sessions.seed_long_term(&profile_key, &mut memory)?;
    let entries = parse_initial_context_entries(initial_context);
    if entries.is_empty() {
        memory.set_fact_in_layer(
            "project_context".to_string(),
            initial_context.to_string(),
            MemoryLayer::LongTerm,
        );
    } else {
        for (key, value) in &entries {
            memory.set_fact_in_layer(key.clone(), value.clone(), MemoryLayer::LongTerm);
        }
        let mut profile = sessions.load_profile(agent_id)?;
        for (key, value) in entries {
            if let Some(field) = profile.fields.iter_mut().find(|field| field.key == key) {
                field.value = value;
            } else {
                profile.fields.push(ProfileField {
                    key: key.clone(),
                    question: key,
                    required: false,
                    value,
                });
            }
        }
        profile.updated_at_unix = crate::chat::unix_now();
        sessions.save_profile(agent_id, &profile)?;
    }
    sessions.save_long_term(&profile_key, &memory)
}

fn parse_initial_context_entries(initial_context: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = initial_context
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            return Vec::new();
        };
        let key = normalize_initial_context_key(key);
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Vec::new();
        }
        entries.push((key, value.to_string()));
    }
    entries
}

fn normalize_initial_context_key(key: &str) -> String {
    key.trim()
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn load_agent_long_term_facts(
    sessions: &LocalSessionStore,
    agent_id: &str,
) -> Result<Vec<MemoryFactPayload>, AppError> {
    let profile_key = format!("agent:{agent_id}");
    Ok(sessions
        .load_long_term(&profile_key)?
        .facts
        .into_iter()
        .map(|(key, value)| MemoryFactPayload {
            key,
            value,
            layer: MemoryLayer::LongTerm.to_string(),
        })
        .collect())
}

/// Merge a freshly-built/edited schema into the stored profile, carrying over any
/// already-elicited values for keys that survive.
pub(super) fn merge_profile_schema(
    existing: Vec<ProfileField>,
    schema: Vec<ProfileFieldPayload>,
) -> Vec<ProfileField> {
    let mut merged: Vec<ProfileField> = schema
        .into_iter()
        .filter(|field| !field.key.trim().is_empty())
        .map(|field| {
            let key = field.key.trim().to_string();
            let value = existing
                .iter()
                .find(|old| old.key == key)
                .map(|old| old.value.clone())
                .unwrap_or_default();
            ProfileField {
                key,
                question: field.question.trim().to_string(),
                required: field.required,
                value,
            }
        })
        .collect();
    for old in existing {
        if old.is_filled() && !merged.iter().any(|field| field.key == old.key) {
            merged.push(old);
        }
    }
    merged
}

fn require_agent_id(id: Option<&str>) -> Result<String, AppError> {
    id.and_then(blank_str_to_none)
        .map(|value| value.to_string())
        .ok_or_else(|| AppError::InvalidInput("Agent id is required".to_string()))
}

fn saved_memory_from_web(memory: WebMemoryConfig) -> SavedMemoryConfig {
    let defaults = MemoryConfig::default();
    SavedMemoryConfig {
        strategy: blank_to_none(memory.strategy).unwrap_or_else(|| defaults.strategy.to_string()),
        recent_messages: memory.recent_messages.unwrap_or(defaults.recent_messages),
        summarize_after_messages: memory
            .summarize_after_messages
            .unwrap_or(defaults.summarize_after_messages),
        summary_chunk_messages: memory
            .summary_chunk_messages
            .unwrap_or(defaults.summary_chunk_messages),
        summarize_at_context_percent: memory
            .summarize_at_context_percent
            .unwrap_or(defaults.summarize_at_context_percent),
        summary_prompt: blank_to_none(memory.summary_prompt).unwrap_or(defaults.summary_prompt),
        facts_extraction_prompt: blank_to_none(memory.facts_extraction_prompt)
            .unwrap_or(defaults.facts_extraction_prompt),
        facts_prompt: blank_to_none(memory.facts_prompt).unwrap_or(defaults.facts_prompt),
        active_branch: blank_to_none(memory.active_branch).unwrap_or(defaults.active_branch),
        scoped_auto_route: memory
            .scoped_auto_route
            .unwrap_or(defaults.scoped_auto_route),
        topic_file_routing: memory
            .topic_file_routing
            .unwrap_or(defaults.topic_file_routing),
        topic_drift_guard: memory
            .topic_drift_guard
            .unwrap_or(defaults.topic_drift_guard),
        topic_auto_create: memory
            .topic_auto_create
            .unwrap_or(defaults.topic_auto_create),
        topic_classifier_prompt: blank_to_none(memory.topic_classifier_prompt)
            .unwrap_or(defaults.topic_classifier_prompt),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AgentsManageRequest {
    pub(super) action: String,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) agent: Option<AgentPayload>,
    /// Token override + its provider, used by `build-schema` (mirrors chat).
    #[serde(default)]
    pub(super) token: Option<String>,
    #[serde(default)]
    pub(super) token_provider: Option<String>,
    /// Dialog target, used by `dialog-rename` / `dialog-delete`.
    #[serde(default)]
    pub(super) session_id: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) task: Option<TaskPayload>,
    /// Swarm config, used by `swarm-save`.
    #[serde(default)]
    pub(super) swarm: Option<SwarmConfig>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AgentPayload {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) provider: String,
    #[serde(default)]
    pub(super) base_url: Option<String>,
    #[serde(default)]
    pub(super) model: String,
    #[serde(default)]
    pub(super) system_prompt: Option<String>,
    /// Free-text domain description (drives the interview + on-domain injection).
    #[serde(default)]
    pub(super) domain: Option<String>,
    /// Free-form facts/context to save as durable agent memory at creation time.
    #[serde(default)]
    pub(super) initial_context: Option<String>,
    /// Hard constraints checked against responses.
    #[serde(default)]
    pub(super) invariants: Option<Vec<String>>,
    /// Required field names the user wants the agent to elicit (schema seed).
    #[serde(default)]
    pub(super) seed_fields: Option<Vec<String>>,
    /// Interview schema (optionally edited by the user) to persist on save.
    #[serde(default)]
    pub(super) profile_schema: Option<Vec<ProfileFieldPayload>>,
    #[serde(default)]
    pub(super) memory: Option<WebMemoryConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProfileFieldPayload {
    #[serde(default)]
    pub(super) key: String,
    #[serde(default)]
    pub(super) question: String,
    #[serde(default)]
    pub(super) required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct TaskPayload {
    #[serde(default)]
    pub(super) stage: String,
    #[serde(default)]
    pub(super) current_step: String,
    #[serde(default)]
    pub(super) expected_action: String,
    #[serde(default)]
    pub(super) paused: bool,
    #[serde(default)]
    pub(super) resume_hint: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) goal: String,
    #[serde(default)]
    pub(super) plan: Vec<String>,
    #[serde(default)]
    pub(super) pipeline: Vec<TaskPipelineStage>,
    #[serde(default)]
    pub(super) artifacts: Vec<TaskArtifact>,
    #[serde(default)]
    pub(super) results: Vec<String>,
    #[serde(default)]
    pub(super) notes: String,
}

impl TaskPayload {
    fn into_task_context(self) -> TaskContext {
        TaskContext {
            stage: self.stage.parse().unwrap_or_default(),
            current_step: self.current_step.trim().to_string(),
            expected_action: self.expected_action.trim().to_string(),
            paused: self.paused,
            resume_hint: self.resume_hint.trim().to_string(),
            title: self.title.trim().to_string(),
            goal: self.goal.trim().to_string(),
            plan: clean_lines(self.plan),
            pipeline: self.pipeline,
            artifacts: self.artifacts,
            results: clean_lines(self.results),
            notes: self.notes.trim().to_string(),
            violations: Vec::new(),
            backlog: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub(super) struct AgentsManageResponse {
    pub(super) agents: Vec<AgentSummary>,
    pub(super) agent: Option<SavedAgent>,
    pub(super) task: Option<TaskContext>,
    #[serde(default)]
    pub(super) task_id: String,
    pub(super) profile: Option<AgentProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) long_term_facts: Vec<MemoryFactPayload>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) dialogs: Vec<crate::chat::DialogMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) swarm: Option<SwarmConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MemoryFactPayload {
    pub(super) key: String,
    pub(super) value: String,
    pub(super) layer: String,
}

pub(super) async fn user_profiles_manage(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<UserProfilesManageRequest>,
) -> Result<Json<UserProfilesManageResponse>, WebError> {
    let mut response = UserProfilesManageResponse::default();
    match request.action.as_str() {
        "list" => {
            response.profiles = state.sessions.list_user_profiles()?;
            response.bindings = state.sessions.load_user_profile_bindings()?;
        }
        "load" => {
            let id = require_agent_id(request.id.as_deref())?;
            response.profile = state.sessions.load_user_profile(&id)?;
            response.bindings = state.sessions.load_user_profile_bindings()?;
        }
        "save" => {
            let payload = request.profile.ok_or_else(|| {
                AppError::InvalidInput("User profile payload is required".to_string())
            })?;
            let id = payload
                .id
                .as_deref()
                .and_then(blank_str_to_none)
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let profile = UserProfile {
                id,
                display_name: payload.display_name.trim().to_string(),
                style_preferences: clean_lines(payload.style_preferences),
                format_preferences: clean_lines(payload.format_preferences),
                constraints: clean_lines(payload.constraints),
                language_preferences: clean_lines(payload.language_preferences),
                response_length: payload.response_length.trim().to_string(),
                custom_instructions: payload.custom_instructions.trim().to_string(),
                updated_at_unix: crate::chat::unix_now(),
            };
            state.sessions.save_user_profile(&profile)?;
            response.profile = Some(profile);
            response.profiles = state.sessions.list_user_profiles()?;
            response.bindings = state.sessions.load_user_profile_bindings()?;
        }
        "delete" => {
            let id = require_agent_id(request.id.as_deref())?;
            state.sessions.delete_user_profile(&id)?;
            let mut bindings = state.sessions.load_user_profile_bindings()?;
            if bindings.active_profile_id == id {
                bindings.active_profile_id.clear();
            }
            bindings
                .default_profile_per_agent
                .retain(|_, profile_id| profile_id != &id);
            state.sessions.save_user_profile_bindings(&bindings)?;
            response.profiles = state.sessions.list_user_profiles()?;
            response.bindings = bindings;
        }
        "bind" => {
            let mut bindings = state.sessions.load_user_profile_bindings()?;
            if let Some(active_profile_id) = request.active_profile_id.as_deref() {
                bindings.active_profile_id = blank_str_to_none(active_profile_id)
                    .map(str::to_string)
                    .unwrap_or_default();
            }
            if let Some(agent_id) = request.agent_id.as_deref().and_then(blank_str_to_none) {
                match request
                    .default_profile_id
                    .as_deref()
                    .and_then(blank_str_to_none)
                {
                    Some(profile_id) => {
                        bindings
                            .default_profile_per_agent
                            .insert(agent_id.to_string(), profile_id.to_string());
                    }
                    None => {
                        bindings.default_profile_per_agent.remove(agent_id);
                    }
                }
            }
            state.sessions.save_user_profile_bindings(&bindings)?;
            response.profiles = state.sessions.list_user_profiles()?;
            response.bindings = bindings;
        }
        other => {
            return Err(
                AppError::InvalidInput(format!("Unknown user profile action: {other}")).into(),
            );
        }
    }
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub(super) struct UserProfilesManageRequest {
    pub(super) action: String,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) profile: Option<UserProfilePayload>,
    #[serde(default)]
    pub(super) active_profile_id: Option<String>,
    #[serde(default)]
    pub(super) agent_id: Option<String>,
    #[serde(default)]
    pub(super) default_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UserProfilePayload {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) display_name: String,
    #[serde(default)]
    pub(super) style_preferences: Vec<String>,
    #[serde(default)]
    pub(super) format_preferences: Vec<String>,
    #[serde(default)]
    pub(super) constraints: Vec<String>,
    #[serde(default)]
    pub(super) language_preferences: Vec<String>,
    #[serde(default)]
    pub(super) response_length: String,
    #[serde(default)]
    pub(super) custom_instructions: String,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct UserProfilesManageResponse {
    pub(super) profiles: Vec<UserProfile>,
    pub(super) profile: Option<UserProfile>,
    pub(super) bindings: UserProfileBindings,
}
