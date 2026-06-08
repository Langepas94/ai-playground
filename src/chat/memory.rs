use serde::{Deserialize, Serialize};

use crate::providers::{ChatMessage, Role};

pub const DEFAULT_RECENT_MESSAGES: usize = 12;
pub const DEFAULT_SUMMARIZE_AFTER_MESSAGES: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryConfig {
    pub recent_messages: usize,
    pub summarize_after_messages: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            recent_messages: DEFAULT_RECENT_MESSAGES,
            summarize_after_messages: DEFAULT_SUMMARIZE_AFTER_MESSAGES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentMemory {
    pub session_summary: Option<String>,
    #[serde(default)]
    pub summarized_message_count: usize,
}

impl AgentMemory {
    pub fn build_context(&self, history: &[ChatMessage], config: MemoryConfig) -> Vec<ChatMessage> {
        let mut context = Vec::new();
        context.extend(
            history
                .iter()
                .filter(|message| message.role == Role::System)
                .cloned(),
        );
        if let Some(summary) = self
            .session_summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            context.push(ChatMessage {
                role: Role::System,
                content: format!(
                    "Memory summary for the earlier part of this local agent session:\n{summary}"
                ),
            });
        }
        let recent_start = history.len().saturating_sub(config.recent_messages);
        context.extend(
            history[recent_start..]
                .iter()
                .filter(|message| message.role != Role::System)
                .cloned(),
        );
        context
    }

    pub fn next_summary_range(
        &self,
        history: &[ChatMessage],
        config: MemoryConfig,
    ) -> Option<std::ops::Range<usize>> {
        if history.len() < config.summarize_after_messages {
            return None;
        }
        let end = history.len().saturating_sub(config.recent_messages);
        if end <= self.summarized_message_count {
            return None;
        }
        Some(self.summarized_message_count..end)
    }
}

pub fn format_messages_for_summary(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|message| message.role != Role::System)
        .map(|message| {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            format!("{role}: {}", message.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
        }
    }

    #[test]
    fn memory_context_layers_summary_before_recent_messages() {
        let memory = AgentMemory {
            session_summary: Some("User likes concise answers.".to_string()),
            summarized_message_count: 2,
        };
        let history = vec![
            message(Role::System, "Base system prompt"),
            message(Role::User, "old user"),
            message(Role::Assistant, "old assistant"),
            message(Role::User, "recent user"),
            message(Role::Assistant, "recent assistant"),
        ];

        let context = memory.build_context(
            &history,
            MemoryConfig {
                recent_messages: 2,
                summarize_after_messages: 3,
            },
        );

        assert_eq!(context.len(), 4);
        assert_eq!(context[0].content, "Base system prompt");
        assert!(context[1].content.contains("User likes concise answers."));
        assert_eq!(context[2].content, "recent user");
        assert_eq!(context[3].content, "recent assistant");
    }

    #[test]
    fn memory_summary_range_keeps_recent_window_unsummarized() {
        let memory = AgentMemory {
            session_summary: None,
            summarized_message_count: 1,
        };
        let history = vec![
            message(Role::User, "1"),
            message(Role::Assistant, "2"),
            message(Role::User, "3"),
            message(Role::Assistant, "4"),
            message(Role::User, "5"),
        ];

        let range = memory
            .next_summary_range(
                &history,
                MemoryConfig {
                    recent_messages: 2,
                    summarize_after_messages: 4,
                },
            )
            .expect("summary range");

        assert_eq!(range, 1..3);
    }
}
