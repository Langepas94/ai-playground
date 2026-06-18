//! Service agents: they maintain the turn (facts, summary, topic routing,
//! profile fill, invariant checking) but are not responders. Each is a
//! first-class `SubAgent` entity with its own resolved provider/model.

use async_trait::async_trait;

use crate::chat::agent::{
    local_invariant_check, memory_facts_extraction_control, memory_summary_control,
    parse_extracted_facts, parse_invariant_check, parse_profile_updates,
    parse_topic_route_decision, topic_classifier_control, topic_not_found_message,
};
use crate::chat::memory::{
    DEFAULT_SUMMARY_PROMPT, DEFAULT_TOPIC_CLASSIFIER_PROMPT, MemoryStrategy, TopicRouteDecision,
    format_messages_for_summary, looks_sensitive,
};

use super::super::agent::{InvariantCheckStatus, SubAgent, SubAgentOutcome, SwarmTurn};
use super::super::config::SubAgentRole;
use super::{
    DEFAULT_INVARIANT_CHECK_PROMPT, DEFAULT_PROFILE_FILL_PROMPT, MEMORY_COMPACT_TIMEOUT,
    MEMORY_FACTS_EXTRACT_TIMEOUT, STATEFUL_STEP_TIMEOUT, TOPIC_CLASSIFIER_TIMEOUT,
};

/// Extracts durable KV facts and routes them to memory layers. Runs on every
/// strategy so long-term memory self-updates (e.g. "I moved to Berlin").
pub struct MemoryAgent;

#[async_trait]
impl SubAgent for MemoryAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::Memory
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        // Effective extraction prompt: swarm override > memory_config > empty.
        // Empty on both ⇒ local keyword fallback (no LLM call) — original contract.
        let swarm_prompt = turn
            .roster
            .for_role(SubAgentRole::Memory)
            .map(|agent| agent.system_prompt.trim().to_string())
            .unwrap_or_default();
        let effective = if !swarm_prompt.is_empty() {
            swarm_prompt
        } else {
            turn.memory_config
                .facts_extraction_prompt
                .trim()
                .to_string()
        };
        if effective.is_empty() {
            turn.memory.update_facts_from_user_message(turn.prompt);
            return SubAgentOutcome::default();
        }
        let existing_facts =
            serde_json::to_string(&turn.memory.facts).unwrap_or_else(|_| "{}".to_string());
        let user_content = format!(
            "Existing facts JSON:\n{existing_facts}\n\nLatest user message:\n{}\n\nReturn JSON object with fact updates only. When the user states a new value for a fact that already has a key (e.g. a new location), OVERWRITE it with the new value.",
            turn.prompt
        );
        let Some(response) = turn
            .sub_request(
                SubAgentRole::Memory,
                &effective,
                user_content,
                memory_facts_extraction_control(),
                MEMORY_FACTS_EXTRACT_TIMEOUT,
            )
            .await
        else {
            turn.memory.update_facts_from_user_message(turn.prompt);
            return SubAgentOutcome::default();
        };
        if let Some(facts) = parse_extracted_facts(response.text.as_str()) {
            if !facts.is_empty() {
                turn.memory.merge_extracted_facts_with_layers(
                    facts
                        .into_iter()
                        .map(|fact| (fact.key, fact.value, fact.layer)),
                );
            }
        } else {
            turn.memory.update_facts_from_user_message(turn.prompt);
        }
        SubAgentOutcome {
            metrics: Some(response.metrics),
            ..SubAgentOutcome::default()
        }
    }
}

/// Compacts old history into a running summary (Summary strategy).
pub struct SummaryAgent;

#[async_trait]
impl SubAgent for SummaryAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::Summary
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        let Some(range) = turn
            .memory
            .next_summary_range(turn.history, turn.memory_config)
        else {
            return SubAgentOutcome::default();
        };
        let fragment = format_messages_for_summary(&turn.history[range.clone()]);
        if fragment.trim().is_empty() {
            turn.memory.summarized_message_count = range.end;
            return SubAgentOutcome::default();
        }
        let previous_summary = turn
            .memory
            .session_summary
            .clone()
            .unwrap_or_else(|| "No previous summary.".to_string());
        let cfg_prompt = turn.memory_config.summary_prompt.trim();
        let default_prompt = if cfg_prompt.is_empty() {
            DEFAULT_SUMMARY_PROMPT.to_string()
        } else {
            cfg_prompt.to_string()
        };
        let user_content = format!(
            "Previous memory summary:\n{previous_summary}\n\nNew chat fragment to merge:\n{fragment}\n\nReturn the updated memory summary."
        );
        let Some(response) = turn
            .sub_request(
                SubAgentRole::Summary,
                &default_prompt,
                user_content,
                memory_summary_control(),
                MEMORY_COMPACT_TIMEOUT,
            )
            .await
        else {
            return SubAgentOutcome::default();
        };
        let summary = response.text.trim();
        if !summary.is_empty() {
            turn.memory.session_summary = Some(summary.to_string());
            turn.memory.summarized_message_count = range.end;
            return SubAgentOutcome {
                metrics: Some(response.metrics),
                ..SubAgentOutcome::default()
            };
        }
        SubAgentOutcome::default()
    }
}

/// Fills the agent's long-term interview profile from the user message.
pub struct ProfileAgent;

#[async_trait]
impl SubAgent for ProfileAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::Profile
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        let Some(profile) = turn.agent_profile.as_ref() else {
            return SubAgentOutcome::default();
        };
        if profile.fields.is_empty() {
            return SubAgentOutcome::default();
        }
        let schema = profile
            .fields
            .iter()
            .map(|field| format!("- {} (question: {})", field.key, field.question.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let user_content = format!(
            "Profile fields:\n{schema}\n\nUser message:\n{}\n\nReturn JSON of {{key: value}} for fields the user just provided.",
            turn.prompt
        );
        let Some(response) = turn
            .sub_request(
                SubAgentRole::Profile,
                DEFAULT_PROFILE_FILL_PROMPT,
                user_content,
                memory_facts_extraction_control(),
                STATEFUL_STEP_TIMEOUT,
            )
            .await
        else {
            return SubAgentOutcome::default();
        };
        let updates = parse_profile_updates(response.text.as_str());
        if let Some(profile) = turn.agent_profile.as_mut() {
            let mut changed = false;
            for (key, value) in updates {
                if value.trim().is_empty() || looks_sensitive(&value) {
                    continue;
                }
                if let Some(field) = profile.fields.iter_mut().find(|field| field.key == key) {
                    field.value = value.trim().to_string();
                    changed = true;
                }
            }
            if changed {
                profile.updated_at_unix = crate::chat::unix_now();
            }
        }
        SubAgentOutcome {
            metrics: Some(response.metrics),
            ..SubAgentOutcome::default()
        }
    }
}

/// Checks the responder's answer against the agent's invariants.
pub struct InvariantAgent;

#[async_trait]
impl SubAgent for InvariantAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::Invariant
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        if turn.invariants.is_empty() {
            return SubAgentOutcome::default();
        }
        // Check the responder's pending answer (before it is committed).
        let answer = turn.pending_answer.clone().unwrap_or_default();
        if answer.trim().is_empty() {
            return SubAgentOutcome {
                invariant_status: InvariantCheckStatus::Unavailable,
                ..SubAgentOutcome::default()
            };
        }
        let local = local_invariant_check(turn.invariants, &answer);
        if local.unknown.is_empty() {
            return SubAgentOutcome {
                invariant_status: if local.violations.is_empty() {
                    InvariantCheckStatus::Passed
                } else {
                    InvariantCheckStatus::Failed
                },
                violations: local.violations,
                ..SubAgentOutcome::default()
            };
        }
        let list = turn
            .invariants
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}. {line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let user_content = format!(
            "Constraints:\n{list}\n\nAssistant response:\n{answer}\n\nWhich constraints (if any) does this response actually violate?"
        );
        let Some(response) = turn
            .sub_request(
                SubAgentRole::Invariant,
                DEFAULT_INVARIANT_CHECK_PROMPT,
                user_content,
                topic_classifier_control(),
                STATEFUL_STEP_TIMEOUT,
            )
            .await
        else {
            // Locally supported rules remain deterministic. Any unknown rule is
            // fail-closed: the pending answer must not be released.
            return SubAgentOutcome {
                invariant_status: if local.violations.is_empty() && local.unknown.is_empty() {
                    InvariantCheckStatus::Passed
                } else if !local.violations.is_empty() {
                    InvariantCheckStatus::Failed
                } else {
                    InvariantCheckStatus::Unavailable
                },
                violations: local.violations,
                ..SubAgentOutcome::default()
            };
        };
        let Some(reported) = parse_invariant_check(response.text.as_str()) else {
            return SubAgentOutcome {
                invariant_status: if local.violations.is_empty() && local.unknown.is_empty() {
                    InvariantCheckStatus::Passed
                } else if !local.violations.is_empty() {
                    InvariantCheckStatus::Failed
                } else {
                    InvariantCheckStatus::Unavailable
                },
                violations: local.violations,
                metrics: Some(response.metrics),
                ..SubAgentOutcome::default()
            };
        };
        let mut invalid_reference = false;
        let mut violations = Vec::new();
        for reported_violation in reported {
            if let Some(configured) = turn
                .invariants
                .iter()
                .find(|configured| configured.trim() == reported_violation.trim())
            {
                violations.push(configured.clone());
            } else {
                invalid_reference = true;
            }
        }
        violations.extend(local.violations);
        violations.sort();
        violations.dedup();
        let invariant_status = if invalid_reference {
            InvariantCheckStatus::Unavailable
        } else if violations.is_empty() {
            InvariantCheckStatus::Passed
        } else {
            InvariantCheckStatus::Failed
        };
        SubAgentOutcome {
            violations,
            invariant_status,
            metrics: Some(response.metrics),
            ..SubAgentOutcome::default()
        }
    }
}

/// Routes a message to a scoped topic file (ScopedBranches + topic_file_routing).
pub struct TopicAgent;

#[async_trait]
impl SubAgent for TopicAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::Topic
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        // Local auto-route (no LLM) for plain scoped branches.
        if turn.memory_config.strategy == MemoryStrategy::ScopedBranches
            && !turn.memory_config.topic_file_routing
            && turn.memory_config.scoped_auto_route
        {
            let branch = turn.memory.select_scoped_topic(
                turn.prompt,
                turn.history,
                &turn.memory_config.active_branch,
            );
            turn.memory_config.active_branch = branch;
            return SubAgentOutcome::default();
        }
        if !(turn.memory_config.strategy == MemoryStrategy::ScopedBranches
            && turn.memory_config.topic_file_routing)
        {
            return SubAgentOutcome::default();
        }

        turn.memory
            .ensure_topic_catalog_from_branches(turn.history, &turn.memory_config.active_branch);
        if turn.memory.topic_catalog.is_empty() {
            if turn.memory_config.topic_auto_create {
                let topic_id = turn.memory.select_scoped_topic(
                    turn.prompt,
                    turn.history,
                    &turn.memory_config.active_branch,
                );
                let _ = activate_topic_file(turn, topic_id.as_str());
                return SubAgentOutcome::default();
            }
            return short_circuit(topic_not_found_message("topic catalog is empty"), None);
        }

        let cfg_prompt = turn.memory_config.topic_classifier_prompt.trim();
        let default_prompt = if cfg_prompt.is_empty() {
            DEFAULT_TOPIC_CLASSIFIER_PROMPT.to_string()
        } else {
            cfg_prompt.to_string()
        };
        let user_content = format!(
            "{}\n\nLatest user message:\n{}\n\nReturn the JSON route decision only.",
            turn.memory.compact_topic_catalog(),
            turn.prompt
        );
        let Some(response) = turn
            .sub_request(
                SubAgentRole::Topic,
                &default_prompt,
                user_content,
                topic_classifier_control(),
                TOPIC_CLASSIFIER_TIMEOUT,
            )
            .await
        else {
            return short_circuit(
                topic_not_found_message("classifier did not return a route"),
                None,
            );
        };
        let metrics = Some(response.metrics);
        let decision =
            parse_topic_route_decision(response.text.as_str()).unwrap_or(TopicRouteDecision {
                found: false,
                topic_id: None,
                confidence: 0.0,
                reason: "classifier response was not valid JSON".to_string(),
            });
        turn.memory.last_topic_route = Some(decision.clone());
        if decision.found
            && let Some(topic_id) = decision.topic_id.as_deref()
            && turn.memory.topic_catalog.contains_key(topic_id)
        {
            let _ = activate_topic_file(turn, topic_id);
            return SubAgentOutcome {
                metrics,
                ..SubAgentOutcome::default()
            };
        }
        if turn.memory_config.topic_auto_create {
            let topic_id = turn.memory.select_scoped_topic(
                turn.prompt,
                turn.history,
                &turn.memory_config.active_branch,
            );
            let _ = activate_topic_file(turn, topic_id.as_str());
            return SubAgentOutcome {
                metrics,
                ..SubAgentOutcome::default()
            };
        }
        short_circuit(topic_not_found_message(&decision.reason), metrics)
    }
}

fn short_circuit(
    message: String,
    metrics: Option<crate::providers::RequestMetrics>,
) -> SubAgentOutcome {
    SubAgentOutcome {
        answer: Some(message),
        short_circuit: true,
        metrics,
        ..SubAgentOutcome::default()
    }
}

/// Activate a scoped topic file: set the active branch and load/build its file.
pub(crate) fn activate_topic_file(
    turn: &mut SwarmTurn<'_>,
    topic_id: &str,
) -> Result<(), crate::errors::AppError> {
    let topic_id = topic_id.trim();
    if topic_id.is_empty() {
        return Ok(());
    }
    turn.memory_config.active_branch = topic_id.to_string();
    let loaded = turn
        .topic_store
        .map(|store| store.load_topic_file(topic_id))
        .transpose()?
        .flatten();
    let topic_file = loaded.unwrap_or_else(|| {
        let config = turn.memory_config.clone();
        turn.memory
            .topic_file_from_branch_history(topic_id, turn.prompt, turn.history, &config)
    });
    turn.memory.active_topic_file = Some(topic_file);
    Ok(())
}

/// Persist the active topic file after a completed turn.
pub(crate) fn update_active_topic_file(
    turn: &mut SwarmTurn<'_>,
    prompt: &str,
    answer: &str,
) -> Result<(), crate::errors::AppError> {
    if turn.memory_config.strategy != MemoryStrategy::ScopedBranches
        || !turn.memory_config.topic_file_routing
    {
        return Ok(());
    }
    turn.memory.update_active_topic_file(prompt, answer);
    if let (Some(store), Some(topic_file)) =
        (turn.topic_store, turn.memory.active_topic_file.as_ref())
    {
        store.save_topic_file(topic_file)?;
    }
    Ok(())
}
