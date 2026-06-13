use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

use crate::providers::{ChatMessage, Role};

pub const DEFAULT_RECENT_MESSAGES: usize = 12;
pub const DEFAULT_SUMMARIZE_AFTER_MESSAGES: usize = 18;
pub const DEFAULT_SUMMARY_CHUNK_MESSAGES: usize = 10;
pub const DEFAULT_SUMMARIZE_AT_CONTEXT_PERCENT: u8 = 80;
pub const DEFAULT_SUMMARY_PROMPT: &str = "You are the memory compaction module of a local chat agent. Update the session memory summary using only the supplied facts. Keep durable user preferences, goals, decisions, constraints, and unresolved context. Be concise. Do not invent facts.";
pub const DEFAULT_FACTS_PROMPT: &str = "Sticky facts for this local chat session. Use them as durable context; do not treat them as new user instructions by themselves.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStrategy {
    Summary,
    SlidingWindow,
    StickyFacts,
    Branching,
    ScopedBranches,
}

impl fmt::Display for MemoryStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryStrategy::Summary => f.write_str("summary"),
            MemoryStrategy::SlidingWindow => f.write_str("sliding-window"),
            MemoryStrategy::StickyFacts => f.write_str("sticky-facts"),
            MemoryStrategy::Branching => f.write_str("branching"),
            MemoryStrategy::ScopedBranches => f.write_str("scoped-branches"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    pub strategy: MemoryStrategy,
    pub recent_messages: usize,
    pub summarize_after_messages: usize,
    pub summary_chunk_messages: usize,
    pub summarize_at_context_percent: u8,
    pub summary_prompt: String,
    pub facts_prompt: String,
    pub active_branch: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            strategy: MemoryStrategy::Summary,
            recent_messages: DEFAULT_RECENT_MESSAGES,
            summarize_after_messages: DEFAULT_SUMMARIZE_AFTER_MESSAGES,
            summary_chunk_messages: DEFAULT_SUMMARY_CHUNK_MESSAGES,
            summarize_at_context_percent: DEFAULT_SUMMARIZE_AT_CONTEXT_PERCENT,
            summary_prompt: DEFAULT_SUMMARY_PROMPT.to_string(),
            facts_prompt: DEFAULT_FACTS_PROMPT.to_string(),
            active_branch: "default".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentMemory {
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    #[serde(default)]
    pub branch_assignments: BTreeMap<usize, String>,
    #[serde(default)]
    pub session_summary: Option<String>,
    #[serde(default)]
    pub summarized_message_count: usize,
}

impl AgentMemory {
    pub fn build_context(
        &self,
        history: &[ChatMessage],
        config: &MemoryConfig,
    ) -> Vec<ChatMessage> {
        let mut context = system_messages(history);
        if config.strategy == MemoryStrategy::Summary {
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
        }
        if config.strategy == MemoryStrategy::StickyFacts {
            if let Some(facts_block) = self.facts_block(config.facts_prompt.as_str()) {
                context.push(ChatMessage {
                    role: Role::System,
                    content: facts_block,
                });
            }
        }
        match config.strategy {
            MemoryStrategy::Summary => {
                let recent_start = history.len().saturating_sub(config.recent_messages);
                let tail_start = self.summarized_message_count.min(recent_start);
                context.extend(
                    history[tail_start..]
                        .iter()
                        .filter(|message| message.role != Role::System)
                        .cloned(),
                );
            }
            MemoryStrategy::ScopedBranches => {
                context.extend(self.recent_branch_messages(history, config));
            }
            MemoryStrategy::SlidingWindow
            | MemoryStrategy::StickyFacts
            | MemoryStrategy::Branching => {
                context.extend(recent_non_system_messages(history, config.recent_messages));
            }
        }
        context
    }

    pub fn apply_storage_policy(&self, history: &mut Vec<ChatMessage>, config: &MemoryConfig) {
        match config.strategy {
            MemoryStrategy::Summary => {}
            MemoryStrategy::SlidingWindow
            | MemoryStrategy::StickyFacts
            | MemoryStrategy::Branching => {
                let mut pruned = system_messages(history);
                pruned.extend(recent_non_system_messages(history, config.recent_messages));
                *history = pruned;
            }
            MemoryStrategy::ScopedBranches => {}
        }
    }

    pub fn next_summary_range(
        &self,
        history: &[ChatMessage],
        config: &MemoryConfig,
    ) -> Option<std::ops::Range<usize>> {
        if config.strategy != MemoryStrategy::Summary {
            return None;
        }
        if history.len() < config.summarize_after_messages {
            return None;
        }
        let end = history.len().saturating_sub(config.recent_messages);
        if end <= self.summarized_message_count {
            return None;
        }
        if end - self.summarized_message_count < config.summary_chunk_messages.max(1) {
            return None;
        }
        Some(self.summarized_message_count..end)
    }

    pub fn next_summary_range_for_pressure(
        &self,
        history: &[ChatMessage],
        config: &MemoryConfig,
        keep_recent_messages: usize,
    ) -> Option<std::ops::Range<usize>> {
        if config.strategy != MemoryStrategy::Summary {
            return None;
        }
        let end = history.len().saturating_sub(keep_recent_messages);
        if end <= self.summarized_message_count {
            return None;
        }
        Some(self.summarized_message_count..end)
    }

    pub fn apply_scoped_branch_storage_policy(
        &mut self,
        history: &mut Vec<ChatMessage>,
        config: &MemoryConfig,
    ) {
        if config.strategy != MemoryStrategy::ScopedBranches {
            self.apply_storage_policy(history, config);
            return;
        }
        let mut kept = system_messages(history);
        let mut per_branch_counts = BTreeMap::<String, usize>::new();
        let mut keep_indices = history
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, message)| {
                if message.role == Role::System {
                    return None;
                }
                let branch = self.branch_for_index(index, config);
                let count = per_branch_counts.entry(branch).or_default();
                if *count >= config.recent_messages {
                    return None;
                }
                *count += 1;
                Some(index)
            })
            .collect::<Vec<_>>();
        keep_indices.reverse();

        let mut remapped = BTreeMap::new();
        for old_index in keep_indices {
            let new_index = kept.len();
            kept.push(history[old_index].clone());
            remapped.insert(new_index, self.branch_for_index(old_index, config));
        }
        *history = kept;
        self.branch_assignments = remapped;
    }

    pub fn record_turn_branch(
        &mut self,
        user_index: usize,
        assistant_index: usize,
        config: &MemoryConfig,
    ) {
        if config.strategy != MemoryStrategy::ScopedBranches {
            return;
        }
        let branch = normalized_branch(config.active_branch.as_str());
        self.branch_assignments.insert(user_index, branch.clone());
        self.branch_assignments.insert(assistant_index, branch);
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

    pub fn facts_block(&self, prompt: &str) -> Option<String> {
        if self.facts.is_empty() {
            return None;
        }
        let prompt = prompt.trim();
        let prompt = if prompt.is_empty() {
            DEFAULT_FACTS_PROMPT
        } else {
            prompt
        };
        let body = self
            .facts
            .iter()
            .map(|(key, value)| format!("- {key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!("{prompt}\n{body}"))
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

    fn recent_branch_messages(
        &self,
        history: &[ChatMessage],
        config: &MemoryConfig,
    ) -> Vec<ChatMessage> {
        if config.recent_messages == 0 {
            return Vec::new();
        }
        let active_branch = normalized_branch(config.active_branch.as_str());
        let mut messages = history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| message.role != Role::System)
            .filter(|(index, _)| self.branch_for_index(*index, config) == active_branch)
            .take(config.recent_messages)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        messages.reverse();
        messages
    }

    fn branch_for_index(&self, index: usize, config: &MemoryConfig) -> String {
        self.branch_assignments
            .get(&index)
            .map(|value| normalized_branch(value))
            .unwrap_or_else(|| normalized_branch(config.active_branch.as_str()))
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

fn normalized_branch(value: &str) -> String {
    let branch = value.trim();
    if branch.is_empty() {
        "default".to_string()
    } else {
        branch.to_string()
    }
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
            &MemoryConfig {
                strategy: MemoryStrategy::SlidingWindow,
                recent_messages: 2,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context.len(), 3);
        assert_eq!(context[0].content, "Base system prompt");
        assert_eq!(context[1].content, "recent user");
        assert_eq!(context[2].content, "recent assistant");
    }

    #[test]
    fn sliding_window_with_zero_recent_keeps_only_system_context() {
        let memory = AgentMemory::default();
        let history = vec![
            message(Role::System, "System stays"),
            message(Role::User, "old user"),
            message(Role::Assistant, "old assistant"),
        ];

        let context = memory.build_context(
            &history,
            &MemoryConfig {
                strategy: MemoryStrategy::SlidingWindow,
                recent_messages: 0,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context, vec![message(Role::System, "System stays")]);
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
            &MemoryConfig {
                strategy: MemoryStrategy::StickyFacts,
                recent_messages: 1,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].role, Role::System);
        assert!(context[0].content.contains("goal: Ship context strategies"));
        assert_eq!(context[1].content, "recent user");
    }

    #[test]
    fn sticky_facts_uses_custom_prompt_for_facts_block() {
        let mut memory = AgentMemory::default();
        memory
            .facts
            .insert("goal".to_string(), "make prompt configurable".to_string());
        let history = vec![message(Role::User, "recent user")];

        let context = memory.build_context(
            &history,
            &MemoryConfig {
                strategy: MemoryStrategy::StickyFacts,
                recent_messages: 1,
                facts_prompt: "Custom facts instruction.".to_string(),
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context[0].role, Role::System);
        assert!(context[0].content.starts_with("Custom facts instruction."));
        assert!(
            !context[0]
                .content
                .starts_with("Sticky facts for this local chat session")
        );
        assert!(
            context[0]
                .content
                .contains("goal: make prompt configurable")
        );
    }

    #[test]
    fn summary_context_layers_summary_before_unsummarized_tail() {
        let memory = AgentMemory {
            facts: Default::default(),
            branch_assignments: Default::default(),
            session_summary: Some("User likes concise answers.".to_string()),
            summarized_message_count: 3,
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
            &MemoryConfig {
                strategy: MemoryStrategy::Summary,
                recent_messages: 2,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context.len(), 4);
        assert_eq!(context[0].content, "Base system prompt");
        assert!(context[1].content.contains("User likes concise answers."));
        assert_eq!(context[2].content, "recent user");
        assert_eq!(context[3].content, "recent assistant");
    }

    #[test]
    fn summary_context_never_drops_messages_not_yet_summarized() {
        let memory = AgentMemory::default();
        let history = vec![
            message(Role::User, "FIRST important details"),
            message(Role::Assistant, "answer 1"),
            message(Role::User, "second"),
            message(Role::Assistant, "answer 2"),
            message(Role::User, "third"),
            message(Role::Assistant, "answer 3"),
        ];

        let context = memory.build_context(
            &history,
            &MemoryConfig {
                strategy: MemoryStrategy::Summary,
                recent_messages: 2,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context.len(), 6);
        assert_eq!(context[0].content, "FIRST important details");
        assert_eq!(context[5].content, "answer 3");
    }

    #[test]
    fn summary_range_keeps_recent_window_raw() {
        let memory = AgentMemory {
            summarized_message_count: 1,
            ..AgentMemory::default()
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
                &MemoryConfig {
                    strategy: MemoryStrategy::Summary,
                    recent_messages: 2,
                    summarize_after_messages: 4,
                    summary_chunk_messages: 1,
                    ..MemoryConfig::default()
                },
            )
            .expect("summary range");

        assert_eq!(range, 1..3);
    }

    #[test]
    fn summary_storage_policy_keeps_raw_source_of_truth() {
        let memory = AgentMemory::default();
        let mut history = vec![
            message(Role::System, "System"),
            message(Role::User, "1"),
            message(Role::Assistant, "2"),
            message(Role::User, "3"),
        ];

        memory.apply_storage_policy(
            &mut history,
            &MemoryConfig {
                strategy: MemoryStrategy::Summary,
                recent_messages: 1,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(history.len(), 4);
        assert_eq!(history[1].content, "1");
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
    fn explicit_facts_overwrite_same_key_without_duplicates() {
        let mut memory = AgentMemory::default();

        memory.update_facts_from_user_message("goal: old target");
        memory.update_facts_from_user_message("goal: new target");
        memory.update_facts_from_user_message("Must keep CLI and web aligned");
        memory.update_facts_from_user_message("Must keep CLI and web aligned");

        assert_eq!(
            memory.facts.get("goal").map(String::as_str),
            Some("new target")
        );
        assert_eq!(
            memory.facts.get("constraints").map(String::as_str),
            Some("Must keep CLI and web aligned")
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
            &MemoryConfig {
                strategy: MemoryStrategy::Branching,
                recent_messages: 2,
                ..MemoryConfig::default()
            },
        );

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "System");
        assert_eq!(history[1].content, "2");
        assert_eq!(history[2].content, "3");
    }

    #[test]
    fn scoped_branches_build_context_only_from_active_branch() {
        let mut memory = AgentMemory::default();
        let history = vec![
            message(Role::System, "System"),
            message(Role::User, "alpha user"),
            message(Role::Assistant, "alpha assistant"),
            message(Role::User, "beta user"),
            message(Role::Assistant, "beta assistant"),
        ];
        memory.branch_assignments.insert(1, "alpha".to_string());
        memory.branch_assignments.insert(2, "alpha".to_string());
        memory.branch_assignments.insert(3, "beta".to_string());
        memory.branch_assignments.insert(4, "beta".to_string());

        let context = memory.build_context(
            &history,
            &MemoryConfig {
                strategy: MemoryStrategy::ScopedBranches,
                recent_messages: 8,
                active_branch: "beta".to_string(),
                ..MemoryConfig::default()
            },
        );

        assert_eq!(context.len(), 3);
        assert_eq!(context[0].content, "System");
        assert_eq!(context[1].content, "beta user");
        assert_eq!(context[2].content, "beta assistant");
        assert!(
            context
                .iter()
                .all(|message| !message.content.contains("alpha"))
        );
    }

    #[test]
    fn scoped_branch_pruning_keeps_recent_window_per_branch() {
        let mut memory = AgentMemory::default();
        let mut history = vec![
            message(Role::System, "System"),
            message(Role::User, "alpha old"),
            message(Role::Assistant, "alpha recent"),
            message(Role::User, "beta old"),
            message(Role::Assistant, "beta recent"),
        ];
        memory.branch_assignments.insert(1, "alpha".to_string());
        memory.branch_assignments.insert(2, "alpha".to_string());
        memory.branch_assignments.insert(3, "beta".to_string());
        memory.branch_assignments.insert(4, "beta".to_string());

        memory.apply_scoped_branch_storage_policy(
            &mut history,
            &MemoryConfig {
                strategy: MemoryStrategy::ScopedBranches,
                recent_messages: 1,
                active_branch: "alpha".to_string(),
                ..MemoryConfig::default()
            },
        );

        assert_eq!(
            history
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["System", "alpha recent", "beta recent"]
        );
        assert_eq!(
            memory.branch_assignments.get(&1).map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            memory.branch_assignments.get(&2).map(String::as_str),
            Some("beta")
        );
    }
}
