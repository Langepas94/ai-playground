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

use super::memory::AgentMemory;

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
            session_summary: Some("User prefers short technical answers.".to_string()),
            summarized_message_count: 8,
        };

        store
            .save_memory(&session.id, &memory)
            .expect("save memory");
        let loaded = store.load_memory(&session.id).expect("load memory");

        assert_eq!(loaded, memory);
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
