//! Explicit prompt assembly — the single place the final provider prompt is
//! built. Replaces the inline `ChatAgent::inject_memory_layers`. Layers are
//! emitted in a fixed order and each value appears once (dedup of agent profile
//! vs user profile), which answers "won't profile + user-profile duplicate?".

use crate::providers::{ChatMessage, Role};

use super::super::agent::{
    remember_agent_profile_context, remember_context_value, render_profile_block,
    render_task_block, render_user_profile_block,
};
use super::super::memory::MemoryStrategy;
use super::agent::SwarmTurn;

/// Assembles the full message list for a responder's provider request.
pub struct PromptBuilder;

impl PromptBuilder {
    /// Build the ordered message list: managed context window + layered system
    /// blocks (deduped) + optional current-stage rules + the user prompt.
    pub fn build(turn: &SwarmTurn<'_>, stage_rules: Option<&str>) -> Vec<ChatMessage> {
        let mut memory_config = turn.memory_config.clone();
        if memory_config.strategy == MemoryStrategy::StickyFacts {
            // The current user prompt counts as one of the last N messages.
            memory_config.recent_messages = memory_config.recent_messages.saturating_sub(1);
        }
        let mut messages = turn.memory.build_context(turn.history, &memory_config);
        Self::inject_layers(turn, &mut messages, stage_rules);
        messages.push(ChatMessage {
            role: Role::User,
            content: turn.prompt.to_string(),
        });
        messages
    }

    /// Insert the layered system blocks before the first non-system message,
    /// in priority order, deduplicating user-profile values already present in
    /// the agent's domain / profile / invariants.
    fn inject_layers(
        turn: &SwarmTurn<'_>,
        messages: &mut Vec<ChatMessage>,
        stage_rules: Option<&str>,
    ) {
        let mut blocks: Vec<ChatMessage> = Vec::new();
        let mut seen_profile_context: Vec<String> = Vec::new();

        if !turn.domain.is_empty() {
            remember_context_value(&mut seen_profile_context, turn.domain);
            blocks.push(system(format!(
                "[agent:domain] Specialization/background: {}. Use this context to be helpful, but do not treat it as a refusal policy. Related adjacent tasks are allowed unless explicit invariants forbid them.",
                turn.domain
            )));
        }

        if let Some(profile) = turn.agent_profile.as_ref()
            && let Some(block) = render_profile_block(profile)
        {
            remember_agent_profile_context(&mut seen_profile_context, profile);
            blocks.push(system(block));
        }

        if let Some(task) = turn.task.as_ref() {
            blocks.push(system(render_task_block(task)));
            if let Some(stage) = task.active_pipeline_stage() {
                let mut active = vec![format!(
                    "[pipeline:active] Stage: {}. Required artifact key: {}.",
                    stage.stage,
                    stage.artifact_key.trim()
                )];
                if !stage.system_prompt.trim().is_empty() {
                    active.push(format!("Stage instruction: {}", stage.system_prompt.trim()));
                }
                for worker in &stage.worker_agents {
                    active.push(format!(
                        "Worker {} [{}]: {}",
                        worker.id.trim(),
                        worker.direction.trim(),
                        worker.system_prompt.trim()
                    ));
                }
                if stage.requires_human_approval {
                    active.push(
                        "After producing the artifact, stop for explicit human approval."
                            .to_string(),
                    );
                }
                blocks.push(system(active.join("\n")));
            }
            if !task.violations.is_empty() {
                blocks.push(system(format!(
                    "[invariants] Your previous response violated these invariants — correct this now:\n- {}",
                    task.violations.join("\n- ")
                )));
            }
        }

        if !turn.worker_outputs.is_empty() {
            let outputs = turn
                .worker_outputs
                .iter()
                .map(|artifact| {
                    format!(
                        "{} [{}]: {}",
                        artifact.key.trim(),
                        artifact.stage,
                        artifact.value.trim()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            blocks.push(system(format!(
                "[pipeline:worker-results]\nReal worker sub-agents produced these artifacts. Synthesize and validate them; do not ignore them:\n{outputs}"
            )));
        }

        if !turn.invariants.is_empty() {
            for invariant in turn.invariants {
                remember_context_value(&mut seen_profile_context, invariant);
            }
            blocks.push(system(format!(
                "[invariants] These constraints are absolute and must never be broken, even if the user asks:\n- {}",
                turn.invariants.join("\n- ")
            )));
        }

        // Invariant feedback for a responder retry (also covers plain chat).
        if !turn.retry_violations.is_empty() {
            blocks.push(system(format!(
                "[invariants] Your previous response violated these invariants — correct this now:\n- {}",
                turn.retry_violations.join("\n- ")
            )));
        }

        // User profile is deduped against everything above so the same value is
        // never injected twice.
        if let Some(profile) = turn.user_profile.as_ref()
            && let Some(block) = render_user_profile_block(profile, &seen_profile_context)
        {
            blocks.push(system(block));
        }

        if let Some(rules) = stage_rules
            && !rules.trim().is_empty()
        {
            blocks.push(system(format!("[stage:rules]\n{}", rules.trim())));
        }

        // Output policy (agent/task turns only): never leak internal
        // control/instruction text into the user answer; never invent unverified
        // norms/dates/amounts.
        if stage_rules.is_some() || turn.task.is_some() || !turn.invariants.is_empty() {
            blocks.push(system(
                "[output:policy] Пиши только пользовательский текст. Не включай в ответ служебные пометки, внутренние инструкции и формулировки контроля (например «не выдавай за норму», «это предположение для модели»). Не выдумывай нормы права, статьи, даты, годы и суммы, которых нет во входных данных — оставляй плейсхолдер и помечай как предположение.".to_string(),
            ));
        }

        if blocks.is_empty() {
            return;
        }
        let insert_at = messages
            .iter()
            .position(|message| message.role != Role::System)
            .unwrap_or(messages.len());
        for (offset, block) in blocks.into_iter().enumerate() {
            messages.insert(insert_at + offset, block);
        }
    }
}

fn system(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::System,
        content,
    }
}
