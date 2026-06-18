//! Responder agents: the four task-stage agents and the general agent. The
//! orchestrator (deterministic) picks exactly one responder per turn based on
//! the current FSM stage. A stage agent signals only "my deliverable is ready"
//! via a machine marker; it never names the next stage — the orchestrator
//! advances the FSM in code.

use async_trait::async_trait;

use crate::providers::ChatRequest;

use super::super::SwarmRunRecord;
use super::super::agent::{SubAgent, SubAgentOutcome, SwarmTurn};
use super::super::config::SubAgentRole;
use super::super::prompt_builder::PromptBuilder;

/// Marker a stage agent appends when its stage deliverable is complete. Parsed
/// in code; the orchestrator then advances the FSM via `allowed_next`.
pub(crate) const STAGE_DONE_MARKER: &str = "<<STAGE_DONE>>";

/// One task-stage responder. The same struct backs all four stages; `role`
/// selects the stage rules and the resolved provider/model.
pub struct StageAgent {
    pub role: SubAgentRole,
}

impl StageAgent {
    pub fn new(role: SubAgentRole) -> Self {
        debug_assert!(role.is_stage());
        Self { role }
    }
}

/// The stage's job description, injected as `[stage:rules]`. Public so the
/// orchestrator can build the same prompt for the streaming path.
pub(crate) fn stage_rules(role: SubAgentRole) -> Option<&'static str> {
    match role {
        SubAgentRole::Planning => Some(
            "Ты на стадии PLANNING. Собери требования и предложи утверждённый план. Не пиши финальный код. Когда план готов и согласован — заверши ответ ОТДЕЛЬНОЙ последней строкой <<STAGE_DONE>>.",
        ),
        SubAgentRole::Execution => Some(
            "Ты на стадии EXECUTION. Работай в рамках текущего шага плана. Если данных не хватает — НЕ блокируй подготовку: дай полезный черновик с явными плейсхолдерами в квадратных скобках (например [номер договора], [дата], [сумма]) и отдельным списком 'что проверить'. Неизвестные значения (даты, номера, суммы, нормы права) оставляй плейсхолдером, не выдумывай. Не переноси известный срок оплаты по договору на срок исполнения претензии: если срок требования пользователь не назвал, пиши [срок требования]. Не добавляй придуманные примеры дат внутрь плейсхолдеров. Когда черновик готов — заверши ответ ОТДЕЛЬНОЙ последней строкой <<STAGE_DONE>>.",
        ),
        SubAgentRole::Validation => Some(
            "Ты на стадии VALIDATION. Проверь результат: риски, пробелы, соответствие плану. Отдели подтверждённые факты от предположений. Когда проверка пройдена — заверши ответ ОТДЕЛЬНОЙ последней строкой <<STAGE_DONE>>.",
        ),
        SubAgentRole::Done => Some(
            "Ты на стадии DONE. Кратко перечисли: что готово, что отправляем, какие приложения, что проверить перед отправкой. Задача завершена, дальнейших переходов нет.",
        ),
        _ => None,
    }
}

#[async_trait]
impl SubAgent for StageAgent {
    fn role(&self) -> SubAgentRole {
        self.role
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        respond(turn, self.role, stage_rules(self.role)).await
    }
}

/// Default responder for plain chat (no active task).
pub struct GeneralAgent;

#[async_trait]
impl SubAgent for GeneralAgent {
    fn role(&self) -> SubAgentRole {
        SubAgentRole::General
    }

    async fn run(&self, turn: &mut SwarmTurn<'_>) -> SubAgentOutcome {
        respond(turn, SubAgentRole::General, None).await
    }
}

/// Shared responder body: build the prompt, call the role's resolved model,
/// parse the stage-done marker deterministically.
async fn respond(
    turn: &mut SwarmTurn<'_>,
    role: SubAgentRole,
    stage_rules: Option<&str>,
) -> SubAgentOutcome {
    let (profile, token) = turn.responder_profile(role);
    let messages = PromptBuilder::build(turn, stage_rules);
    let request = ChatRequest {
        model: profile.model.clone(),
        messages,
        control: turn.control.clone(),
        pricing: turn.pricing.clone(),
        billing: turn.billing.clone(),
    };
    let mut record = SwarmRunRecord::new(role, profile.model.clone());
    let response = match turn
        .client
        .chat_completion_with_debug(&profile, &token, request)
        .await
    {
        Ok((response, debug)) => {
            turn.captured_debug = Some(debug);
            response
        }
        Err(_) => {
            record.note = "responder error".to_string();
            turn.report.record(record);
            return SubAgentOutcome::default();
        }
    };
    record.ran = true;
    record.metrics = Some(response.metrics.clone());
    turn.report.record(record);

    let (answer, complete, key) = strip_stage_marker(&response.text);
    let artifact = if complete {
        // Pipeline tasks need the stage's expected artifact key; the model may
        // name it in the marker, else default to the stage role name.
        let key = if key.is_empty() {
            role.as_str().to_string()
        } else {
            key
        };
        Some((key, truncate(&answer, 280)))
    } else {
        None
    };
    SubAgentOutcome {
        answer: Some(answer),
        stage_complete: complete && role.is_stage(),
        artifact,
        metrics: Some(response.metrics),
        ..SubAgentOutcome::default()
    }
}

/// Remove a `<<STAGE_DONE>>` / `<<STAGE_DONE: artifact_key>>` marker from the
/// answer. Returns `(clean_answer, completed, artifact_key)`. Public so the
/// streaming finalize path parses it identically.
pub(crate) fn strip_stage_marker(text: &str) -> (String, bool, String) {
    let Some(index) = text.find(STAGE_DONE_MARKER) else {
        return (text.trim().to_string(), false, String::new());
    };
    let after = &text[index + STAGE_DONE_MARKER.len()..];
    // Optional "STAGE_DONE: key>>" form (marker stored without the trailing >>).
    let (key, tail_start) = if let Some(stripped) = after.strip_prefix(':') {
        match stripped.find(">>") {
            Some(end) => (
                stripped[..end].trim().to_string(),
                index + STAGE_DONE_MARKER.len() + 1 + end + 2,
            ),
            None => (String::new(), index + STAGE_DONE_MARKER.len()),
        }
    } else {
        (String::new(), index + STAGE_DONE_MARKER.len())
    };
    let mut cleaned = String::with_capacity(text.len());
    cleaned.push_str(&text[..index]);
    cleaned.push_str(&text[tail_start..]);
    (cleaned.trim().to_string(), true, key)
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}
