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
    providers::{ChatMessage, Role},
};

use super::memory::AgentMemory;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSession {
    pub id: String,
    pub messages: Vec<ChatMessage>,
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
        })
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
        })
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
    use crate::providers::Role;

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
    }

    #[test]
    fn local_session_store_roundtrips_memory_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalSessionStore::from_root(dir.path().join("sessions"));
        let session = store.create_session().expect("create session");
        let memory = AgentMemory {
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
