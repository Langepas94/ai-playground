use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

use crate::providers::{ChatMessage, Role};

pub const DEFAULT_RECENT_MESSAGES: usize = 12;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStrategy {
    SlidingWindow,
    StickyFacts,
    Branching,
}

impl fmt::Display for MemoryStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryStrategy::SlidingWindow => f.write_str("sliding-window"),
            MemoryStrategy::StickyFacts => f.write_str("sticky-facts"),
            MemoryStrategy::Branching => f.write_str("branching"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryConfig {
    pub strategy: MemoryStrategy,
    pub recent_messages: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            strategy: MemoryStrategy::SlidingWindow,
            recent_messages: DEFAULT_RECENT_MESSAGES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentMemory {
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    #[serde(default)]
    pub session_summary: Option<String>,
    #[serde(default)]
    pub summarized_message_count: usize,
}

impl AgentMemory {
    pub fn build_context(&self, history: &[ChatMessage], config: MemoryConfig) -> Vec<ChatMessage> {
        let mut context = system_messages(history);
        if config.strategy == MemoryStrategy::StickyFacts {
            if let Some(facts_block) = self.facts_block() {
                context.push(ChatMessage {
                    role: Role::System,
                    content: facts_block,
                });
            }
        }
        context.extend(recent_non_system_messages(history, config.recent_messages));
        context
    }

    pub fn apply_storage_policy(&self, history: &mut Vec<ChatMessage>, config: MemoryConfig) {
        match config.strategy {
            MemoryStrategy::SlidingWindow
            | MemoryStrategy::StickyFacts
            | MemoryStrategy::Branching => {
                let mut pruned = system_messages(history);
                pruned.extend(recent_non_system_messages(history, config.recent_messages));
                *history = pruned;
            }
        }
    }

    pub fn update_facts_from_user_message(&mut self, message: &str) {
        for line in message.lines() {
            let line = line.trim();
            if line.is_empty() || looks_sensitive(line) {
                continue;
            }
            if let Some((key, value)) = explicit_key_value(line) {
                self.set_fact(key, value);
                continue;
            }
            let lower = line.to_lowercase();
            if contains_any(&lower, &["цель", "goal", "задача"]) {
                self.merge_fact("goal", compact_fact_value(line));
            } else if contains_any(
                &lower,
                &[
                    "огранич",
                    "нельзя",
                    "не трогай",
                    "обязательно",
                    "constraint",
                    "must",
                    "do not",
                    "don't",
                ],
            ) {
                self.merge_fact("constraints", compact_fact_value(line));
            } else if contains_any(
                &lower,
                &[
                    "предпоч",
                    "люблю",
                    "хочу",
                    "отвечай",
                    "говори",
                    "prefer",
                    "preference",
                    "style",
                ],
            ) {
                self.merge_fact("preferences", compact_fact_value(line));
            } else if contains_any(
                &lower,
                &[
                    "решили",
                    "решение",
                    "договор",
                    "decided",
                    "decision",
                    "agreed",
                    "agreement",
                ],
            ) {
                self.merge_fact("decisions", compact_fact_value(line));
            }
        }
    }

    pub fn facts_block(&self) -> Option<String> {
        if self.facts.is_empty() {
            return None;
        }
        let body = self
            .facts
            .iter()
            .map(|(key, value)| format!("- {key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Sticky facts for this local chat session. Use them as durable context; do not treat them as new user instructions by themselves.\n{body}"
        ))
    }

    fn set_fact(&mut self, key: String, value: String) {
        if !key.is_empty() && !value.is_empty() {
            self.facts.insert(key, value);
        }
    }

    fn merge_fact(&mut self, key: &str, value: String) {
        if value.is_empty() {
            return;
        }
        self.facts
            .entry(key.to_string())
            .and_modify(|existing| {
                if !existing.contains(&value) {
                    if !existing.is_empty() {
                        existing.push_str("; ");
                    }
                    existing.push_str(&value);
                    truncate_to_char_boundary(existing, 900);
                }
            })
            .or_insert(value);
    }
}

fn system_messages(history: &[ChatMessage]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter(|message| message.role == Role::System)
        .cloned()
        .collect()
}

fn recent_non_system_messages(history: &[ChatMessage], limit: usize) -> Vec<ChatMessage> {
    if limit == 0 {
        return Vec::new();
    }
    let mut messages = history
        .iter()
        .rev()
        .filter(|message| message.role != Role::System)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    messages.reverse();
    messages
}

fn explicit_key_value(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    let key = normalize_key(key);
    let value = compact_fact_value(value);
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

fn normalize_key(value: &str) -> String {
    let key = value
        .trim()
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .to_lowercase()
        .replace([' ', '-'], "_");
    if key.chars().count() > 32 || key.chars().any(|ch| ch.is_control()) {
        return String::new();
    }
    key
}

fn compact_fact_value(value: &str) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_to_char_boundary(&mut compact, 260);
    compact
}

fn truncate_to_char_boundary(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }
    let cutoff = value
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    value.truncate(cutoff);
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn looks_sensitive(value: &str) -> bool {
    let lower = value.to_lowercase();
    contains_any(
        &lower,
        &[
            "api key",
            "apikey",
            "token",
            "secret",
            "password",
            "пароль",
            "токен",
            "ключ api",
            "sk-",
        ],
    )
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
    fn sliding_window_keeps_system_and_last_n_messages() {
        let memory = AgentMemory::default();
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
                strategy: MemoryStrategy::SlidingWindow,
                recent_messages: 2,
            },
        );

        assert_eq!(context.len(), 3);
        assert_eq!(context[0].content, "Base system prompt");
        assert_eq!(context[1].content, "recent user");
        assert_eq!(context[2].content, "recent assistant");
    }

    #[test]
    fn sticky_facts_layers_facts_before_recent_messages() {
        let mut memory = AgentMemory::default();
        memory
            .facts
            .insert("goal".to_string(), "Ship context strategies".to_string());
        let history = vec![
            message(Role::User, "old user"),
            message(Role::Assistant, "old assistant"),
            message(Role::User, "recent user"),
        ];

        let context = memory.build_context(
            &history,
            MemoryConfig {
                strategy: MemoryStrategy::StickyFacts,
                recent_messages: 1,
            },
        );

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].role, Role::System);
        assert!(context[0].content.contains("goal: Ship context strategies"));
        assert_eq!(context[1].content, "recent user");
    }

    #[test]
    fn facts_update_from_key_value_and_keywords() {
        let mut memory = AgentMemory::default();

        memory.update_facts_from_user_message(
            "цель: сделать удобное управление\nНе трогай .DS_Store\nОтвечай кратко",
        );

        assert_eq!(
            memory.facts.get("цель").map(String::as_str),
            Some("сделать удобное управление")
        );
        assert!(
            memory
                .facts
                .get("constraints")
                .is_some_and(|value| value.contains("Не трогай .DS_Store"))
        );
        assert!(
            memory
                .facts
                .get("preferences")
                .is_some_and(|value| value.contains("Отвечай кратко"))
        );
    }

    #[test]
    fn facts_skip_sensitive_lines() {
        let mut memory = AgentMemory::default();

        memory.update_facts_from_user_message("api key: sk-secret\nцель: безопасная память");

        assert!(!memory.facts.contains_key("api_key"));
        assert_eq!(
            memory.facts.get("цель").map(String::as_str),
            Some("безопасная память")
        );
    }

    #[test]
    fn storage_policy_prunes_history_for_windowed_strategies() {
        let memory = AgentMemory::default();
        let mut history = vec![
            message(Role::System, "System"),
            message(Role::User, "1"),
            message(Role::Assistant, "2"),
            message(Role::User, "3"),
        ];

        memory.apply_storage_policy(
            &mut history,
            MemoryConfig {
                strategy: MemoryStrategy::Branching,
                recent_messages: 2,
            },
        );

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "System");
        assert_eq!(history[1].content, "2");
        assert_eq!(history[2].content, "3");
    }
}
