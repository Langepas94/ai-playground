use std::collections::BTreeMap;

use crate::{
    config::ProfileConfig,
    errors::AppError,
    providers::{
        BillingLookup, ChatMessage, ChatRequest, ModelPricing, ProviderClient, ResponseControl,
        ResponseFormat, Role,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConversationStopMode {
    #[default]
    Manual,
    State,
    Instruction,
    Combined,
}

impl std::fmt::Display for ConversationStopMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual => write!(f, "manual"),
            Self::State => write!(f, "state"),
            Self::Instruction => write!(f, "instruction"),
            Self::Combined => write!(f, "combined"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationGoal {
    pub required_fields: Vec<String>,
    pub mode: ConversationStopMode,
}

impl ConversationGoal {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn is_enabled(&self) -> bool {
        !self.required_fields.is_empty() && self.mode != ConversationStopMode::Manual
    }

    pub fn apply_to_control(&self, mut control: ResponseControl) -> ResponseControl {
        if !self.is_enabled() {
            return control;
        }
        control.format = ResponseFormat::JsonObject;
        control.format_instruction = Some(merge_instruction(
            control.format_instruction,
            goal_format_instruction(&self.required_fields),
        ));
        if matches!(
            self.mode,
            ConversationStopMode::Instruction | ConversationStopMode::Combined
        ) {
            control.completion_instruction = Some(merge_instruction(
                control.completion_instruction,
                goal_completion_instruction(&self.required_fields),
            ));
        }
        control
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalState {
    required_fields: Vec<String>,
    values: BTreeMap<String, serde_json::Value>,
    done_signal: bool,
}

impl GoalState {
    pub fn new(required_fields: Vec<String>) -> Self {
        Self {
            required_fields,
            values: BTreeMap::new(),
            done_signal: false,
        }
    }

    pub fn update_from_response(&mut self, text: &str) -> Result<(), AppError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| AppError::Json(error.to_string()))?;
        self.done_signal = value
            .get("done")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let fields = value.get("fields").unwrap_or(&value);
        for field in &self.required_fields {
            if let Some(field_value) = fields.get(field)
                && is_filled(field_value)
            {
                self.values.insert(field.clone(), field_value.clone());
            }
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.required_fields
            .iter()
            .all(|field| self.values.get(field).is_some_and(is_filled))
    }

    pub fn done_signal(&self) -> bool {
        self.done_signal
    }

    pub fn should_stop(&self, mode: ConversationStopMode) -> bool {
        match mode {
            ConversationStopMode::Manual => false,
            ConversationStopMode::State => self.is_complete(),
            ConversationStopMode::Instruction => self.done_signal,
            ConversationStopMode::Combined => self.is_complete() && self.done_signal,
        }
    }

    pub fn summary(&self) -> String {
        let filled = self
            .required_fields
            .iter()
            .filter(|field| self.values.get(*field).is_some_and(is_filled))
            .count();
        format!(
            "{filled}/{} required fields filled; done_signal={}",
            self.required_fields.len(),
            self.done_signal
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalRun {
    pub mode: ConversationStopMode,
    pub response: String,
    pub metrics: crate::providers::RequestMetrics,
    pub state_summary: String,
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalComparison {
    pub state: GoalRun,
    pub instruction: GoalRun,
    pub combined: GoalRun,
}

pub async fn run_goal_once(
    client: &dyn ProviderClient,
    profile: &ProfileConfig,
    token: &str,
    prompt: String,
    required_fields: &[String],
    mode: ConversationStopMode,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
) -> Result<GoalRun, AppError> {
    let goal = ConversationGoal {
        required_fields: required_fields.to_vec(),
        mode,
    };
    let response = client
        .chat_completion(
            profile,
            token,
            ChatRequest {
                model: profile.model.clone(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: prompt,
                }],
                control: goal.apply_to_control(ResponseControl::uncontrolled()),
                pricing,
                billing,
            },
        )
        .await?;
    let mut state = GoalState::new(required_fields.to_vec());
    state.update_from_response(&response.text)?;
    let stopped = state.should_stop(mode);
    Ok(GoalRun {
        mode,
        response: response.text,
        metrics: response.metrics,
        state_summary: state.summary(),
        stopped,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

pub(super) fn goal_format_instruction(required_fields: &[String]) -> String {
    format!(
        "You are collecting a structured entity. Return only a JSON object with this shape: {{\"fields\":{{{}}},\"next_question\":\"string or null\",\"done\":boolean}}. Use null for unknown fields.",
        required_fields
            .iter()
            .map(|field| format!("\"{field}\":null"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(super) fn goal_completion_instruction(required_fields: &[String]) -> String {
    format!(
        "When all required fields are known ({}), set done=true and stop asking follow-up questions. Otherwise set done=false and ask one next_question.",
        required_fields.join(", ")
    )
}

pub(super) fn merge_instruction(existing: Option<String>, added: String) -> String {
    match existing {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}\n{added}"),
        _ => added,
    }
}

pub(super) fn is_filled(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_stop_uses_filled_required_fields() {
        let mut state = GoalState::new(vec!["topic".to_string(), "audience".to_string()]);
        state
            .update_from_response(r#"{"fields":{"topic":"Rust","audience":"junior"},"done":false}"#)
            .expect("update");

        assert!(state.is_complete());
        assert!(state.should_stop(ConversationStopMode::State));
        assert!(!state.should_stop(ConversationStopMode::Instruction));
        assert!(!state.should_stop(ConversationStopMode::Combined));
    }

    #[test]
    fn instruction_stop_trusts_done_signal() {
        let mut state = GoalState::new(vec!["topic".to_string(), "audience".to_string()]);
        state
            .update_from_response(r#"{"fields":{"topic":"Rust","audience":null},"done":true}"#)
            .expect("update");

        assert!(!state.is_complete());
        assert!(!state.should_stop(ConversationStopMode::State));
        assert!(state.should_stop(ConversationStopMode::Instruction));
        assert!(!state.should_stop(ConversationStopMode::Combined));
    }

    #[test]
    fn combined_stop_requires_state_and_done_signal() {
        let mut state = GoalState::new(vec!["topic".to_string()]);
        state
            .update_from_response(r#"{"fields":{"topic":"Rust"},"done":true}"#)
            .expect("update");

        assert!(state.should_stop(ConversationStopMode::State));
        assert!(state.should_stop(ConversationStopMode::Instruction));
        assert!(state.should_stop(ConversationStopMode::Combined));
    }

    /// Инструкция формата включает все required_fields
    #[test]
    fn goal_format_instruction_includes_all_required_fields() {
        let fields = vec!["topic".to_string(), "audience".to_string(), "tone".to_string()];
        let instruction = goal_format_instruction(&fields);

        assert!(instruction.contains("\"topic\""));
        assert!(instruction.contains("\"audience\""));
        assert!(instruction.contains("\"tone\""));
        assert!(instruction.contains("\"done\":boolean"));
    }

    /// merge_instruction добавляет к существующей через перенос строки
    #[test]
    fn merge_instruction_appends_with_newline() {
        let result = merge_instruction(Some("первая".to_string()), "вторая".to_string());
        assert_eq!(result, "первая\nвторая");
    }

    /// merge_instruction с пустой existing → просто added
    #[test]
    fn merge_instruction_with_empty_existing_returns_added() {
        assert_eq!(merge_instruction(None, "added".to_string()), "added");
        assert_eq!(
            merge_instruction(Some("   ".to_string()), "added".to_string()),
            "added"
        );
    }

    /// is_filled корректно обрабатывает разные типы
    #[test]
    fn is_filled_handles_various_json_types() {
        use serde_json::json;

        assert!(!is_filled(&json!(null)));
        assert!(!is_filled(&json!("")));
        assert!(!is_filled(&json!("   ")));
        assert!(!is_filled(&serde_json::Value::Array(vec![])));
        assert!(is_filled(&json!("hello")));
        assert!(is_filled(&json!(42)));
        assert!(is_filled(&json!(false)));
        assert!(is_filled(&json!(["a"])));
    }

    /// GoalState: частичное заполнение не считается complete
    #[test]
    fn goal_state_partial_fill_is_not_complete() {
        let mut state = GoalState::new(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        state
            .update_from_response(r#"{"fields":{"a":"val","b":"val"},"done":false}"#)
            .expect("update");

        assert!(!state.is_complete());
        assert!(!state.should_stop(ConversationStopMode::State));
    }

    /// GoalState не считает null-поля заполненными
    #[test]
    fn goal_state_null_field_is_not_filled() {
        let mut state = GoalState::new(vec!["topic".to_string()]);
        state
            .update_from_response(r#"{"fields":{"topic":null},"done":false}"#)
            .expect("update");

        assert!(!state.is_complete());
    }

    #[test]
    fn goal_control_forces_json_and_preserves_user_instruction() {
        use crate::providers::ResponseFormat;
        let goal = ConversationGoal {
            required_fields: vec!["topic".to_string()],
            mode: ConversationStopMode::Combined,
        };
        let control = goal.apply_to_control(crate::providers::ResponseControl {
            format_instruction: Some("Use concise values.".to_string()),
            ..crate::providers::ResponseControl::uncontrolled()
        });

        assert_eq!(control.format, ResponseFormat::JsonObject);
        assert!(control
            .format_instruction
            .expect("format instruction")
            .contains("Use concise values."));
        assert!(control
            .completion_instruction
            .expect("completion instruction")
            .contains("done=true"));
    }
}
