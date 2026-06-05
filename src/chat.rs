use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use directories::ProjectDirs;

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        AnswerFormat, ChatMessage, ChatRequest, ProviderClient, ResponseControl, ResponseFormat,
        Role,
    },
    secrets::{SecretStore, get_config_profile_token},
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

pub async fn ask_once(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    control: ResponseControl,
) -> Result<crate::providers::ChatResponse, AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let response = client
        .chat_completion(
            profile,
            &token,
            ChatRequest {
                model: profile.model.clone(),
                messages: vec![ChatMessage {
                    role: Role::User,
                    content: prompt,
                }],
                control,
            },
        )
        .await?;
    Ok(response)
}

pub async fn compare_response_control(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    controlled: ResponseControl,
) -> Result<
    (
        crate::providers::ChatResponse,
        crate::providers::ChatResponse,
    ),
    AppError,
> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let base_request = ChatRequest {
        model: profile.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt.clone(),
        }],
        control: ResponseControl::uncontrolled(),
    };
    let controlled_request = ChatRequest {
        model: profile.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt,
        }],
        control: controlled,
    };
    let unrestricted = client
        .chat_completion(profile, &token, base_request)
        .await?;
    let restricted = client
        .chat_completion(profile, &token, controlled_request)
        .await?;
    Ok((unrestricted, restricted))
}

pub async fn compare_goal_stop(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    prompt: String,
    required_fields: Vec<String>,
) -> Result<GoalComparison, AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let state = run_goal_once(
        client,
        profile,
        &token,
        prompt.clone(),
        &required_fields,
        ConversationStopMode::State,
    )
    .await?;
    let instruction = run_goal_once(
        client,
        profile,
        &token,
        prompt.clone(),
        &required_fields,
        ConversationStopMode::Instruction,
    )
    .await?;
    let combined = run_goal_once(
        client,
        profile,
        &token,
        prompt,
        &required_fields,
        ConversationStopMode::Combined,
    )
    .await?;
    Ok(GoalComparison {
        state,
        instruction,
        combined,
    })
}

pub async fn interactive_chat(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    mut control: ResponseControl,
    mut goal: ConversationGoal,
) -> Result<(), AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let mut messages = Vec::<ChatMessage>::new();
    let mut goal_state = GoalState::new(goal.required_fields.clone());
    println!(
        "Chat started with profile '{profile_name}' and model '{}'.",
        profile.model
    );
    println!(
        "Use /exit, /profile, /model, /clear, /save, /control, /format, /answer-format, /max-tokens, /temperature, /top-p, /presence-penalty, /frequency-penalty, /seed, /stop, /goal."
    );

    loop {
        print!("> ");
        io::stdout()
            .flush()
            .map_err(|error| AppError::Terminal(error.to_string()))?;
        let Some(line) = read_terminal_line()? else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "/exit" => break,
            "/profile" => {
                println!("{profile_name}");
                continue;
            }
            "/model" => {
                println!("{}", profile.model);
                continue;
            }
            "/clear" => {
                messages.clear();
                println!("History cleared.");
                continue;
            }
            "/save" => {
                let path = save_history(profile_name, &messages)?;
                println!("Saved to {}", path.display());
                continue;
            }
            "/control" => {
                println!("{}", describe_control(&control));
                continue;
            }
            "/goal" => {
                println!("{}", describe_goal(&goal, &goal_state));
                continue;
            }
            "/control clear" => {
                control = ResponseControl::uncontrolled();
                println!("Response control cleared.");
                continue;
            }
            "/goal clear" => {
                goal = ConversationGoal::disabled();
                goal_state = GoalState::new(Vec::new());
                println!("Conversation goal cleared.");
                continue;
            }
            "/stop clear" => {
                control.stop.clear();
                println!("Stop sequences cleared.");
                continue;
            }
            "/quote-question" => {
                control.quote_question = true;
                println!("Question quoting enabled.");
                continue;
            }
            "/quote-question clear" => {
                control.quote_question = false;
                println!("Question quoting disabled.");
                continue;
            }
            _ => {}
        }

        if let Some(value) = line.strip_prefix("/format ") {
            match value {
                "text" => {
                    control.format = ResponseFormat::Text;
                    println!("Response format: text");
                }
                "json" | "json-object" => {
                    control.format = ResponseFormat::JsonObject;
                    println!("Response format: json-object");
                }
                _ => println!("Use /format text or /format json-object."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/answer-format ") {
            match parse_answer_format(value) {
                Some(format) => {
                    control.answer_format = format;
                    println!("Answer format: {format}");
                }
                None => println!("Use /answer-format natural|bullets|numbered|short|steps|table."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/answer-prefix ") {
            control.answer_prefix = Some(value.to_string());
            println!("Answer prefix updated.");
            continue;
        }

        if let Some(value) = line.strip_prefix("/answer-suffix ") {
            control.answer_suffix = Some(value.to_string());
            println!("Answer suffix updated.");
            continue;
        }

        if let Some(value) = line.strip_prefix("/address-as ") {
            control.address_as = Some(value.to_string());
            println!("Addressing rule updated.");
            continue;
        }

        if let Some(value) = line.strip_prefix("/max-tokens ") {
            match value.parse::<u32>() {
                Ok(max_tokens) => {
                    control.max_tokens = Some(max_tokens);
                    println!("Max tokens: {max_tokens}");
                }
                Err(_) => println!("Use /max-tokens <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/temperature ") {
            match value.parse::<f32>() {
                Ok(temperature) => {
                    control.temperature = Some(temperature);
                    println!("Temperature: {temperature}");
                }
                Err(_) => println!("Use /temperature <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/top-p ") {
            match value.parse::<f32>() {
                Ok(top_p) => {
                    control.top_p = Some(top_p);
                    println!("Top-p: {top_p}");
                }
                Err(_) => println!("Use /top-p <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/presence-penalty ") {
            match value.parse::<f32>() {
                Ok(presence_penalty) => {
                    control.presence_penalty = Some(presence_penalty);
                    println!("Presence penalty: {presence_penalty}");
                }
                Err(_) => println!("Use /presence-penalty <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/frequency-penalty ") {
            match value.parse::<f32>() {
                Ok(frequency_penalty) => {
                    control.frequency_penalty = Some(frequency_penalty);
                    println!("Frequency penalty: {frequency_penalty}");
                }
                Err(_) => println!("Use /frequency-penalty <number>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/seed ") {
            match value.parse::<i64>() {
                Ok(seed) => {
                    control.seed = Some(seed);
                    println!("Seed: {seed}");
                }
                Err(_) => println!("Use /seed <integer>."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/stop ") {
            if value.is_empty() {
                println!("Use /stop <sequence> or /stop clear.");
            } else {
                control.stop.push(value.to_string());
                println!("Stop sequence added.");
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/goal field ") {
            if value.is_empty() {
                println!("Use /goal field <name>.");
            } else if goal.required_fields.iter().any(|field| field == value) {
                println!("Goal field already exists.");
            } else {
                goal.required_fields.push(value.to_string());
                goal_state = GoalState::new(goal.required_fields.clone());
                if goal.mode == ConversationStopMode::Manual {
                    goal.mode = ConversationStopMode::State;
                }
                println!("Goal field added: {value}");
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/goal mode ") {
            match parse_stop_mode(value) {
                Some(mode) => {
                    goal.mode = mode;
                    println!("Goal stop mode: {mode}");
                }
                None => println!("Use /goal mode manual|state|instruction|combined."),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("/completion-instruction ") {
            control.completion_instruction = Some(value.to_string());
            println!("Completion instruction updated.");
            continue;
        }

        if let Some(value) = line.strip_prefix("/format-instruction ") {
            control.format_instruction = Some(value.to_string());
            println!("Format instruction updated.");
            continue;
        }

        messages.push(ChatMessage {
            role: Role::User,
            content: line.to_string(),
        });
        eprintln!("Waiting for provider response...");
        let effective_control = goal.apply_to_control(control.clone());
        let response = client
            .chat_completion(
                profile,
                &token,
                ChatRequest {
                    model: profile.model.clone(),
                    messages: messages.clone(),
                    control: effective_control,
                },
            )
            .await?;
        println!("{}", response.text);
        eprintln!("{}", format_request_metrics(&response.metrics));

        if goal.is_enabled() {
            match goal_state.update_from_response(&response.text) {
                Ok(()) => {
                    println!("Goal state: {}", goal_state.summary());
                    if goal_state.should_stop(goal.mode) {
                        println!("Conversation goal reached by {} stop mode.", goal.mode);
                        break;
                    }
                }
                Err(error) => println!("Goal state was not updated: {error}"),
            }
        }

        messages.push(ChatMessage {
            role: Role::Assistant,
            content: response.text,
        });
    }

    if config.active_profile.as_deref() != Some(profile_name) {
        println!("Session used non-active profile '{profile_name}'.");
    }
    Ok(())
}

pub fn format_request_metrics(metrics: &crate::providers::RequestMetrics) -> String {
    let usage = metrics
        .usage
        .as_ref()
        .map(|usage| {
            format!(
                "tokens: input={} output={} total={}",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            )
        })
        .unwrap_or_else(|| "tokens: unavailable".to_string());
    let cost = metrics
        .cost
        .as_ref()
        .map(|cost| {
            format!(
                "cost: {:.8} {} ({})",
                cost.amount, cost.currency, cost.source
            )
        })
        .unwrap_or_else(|| "cost: unavailable".to_string());
    format!("time: {} ms\n{usage}\n{cost}", metrics.elapsed_ms)
}

fn read_terminal_line() -> Result<Option<String>, AppError> {
    let mut bytes = Vec::new();
    let read = io::stdin()
        .lock()
        .read_until(b'\n', &mut bytes)
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    while bytes
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        bytes.pop();
    }
    Ok(Some(decode_terminal_line(&bytes)))
}

fn decode_terminal_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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

async fn run_goal_once(
    client: &dyn ProviderClient,
    profile: &ProfileConfig,
    token: &str,
    prompt: String,
    required_fields: &[String],
    mode: ConversationStopMode,
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

pub fn describe_control(control: &ResponseControl) -> String {
    if control.is_uncontrolled() {
        return "Response control: none".to_string();
    }
    let stop = if control.stop.is_empty() {
        "none".to_string()
    } else {
        control.stop.join(", ")
    };
    format!(
        "Response control: api_format={}, answer_format={}, max_tokens={}, temperature={}, top_p={}, presence_penalty={}, frequency_penalty={}, seed={}, stop={}, answer_prefix={}, answer_suffix={}, address_as={}, quote_question={}, format_instruction={}, completion_instruction={}",
        control.format,
        control.answer_format,
        control
            .max_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .temperature
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .top_p
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .presence_penalty
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .frequency_penalty
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .seed
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        stop,
        control.answer_prefix.as_deref().unwrap_or("none"),
        control.answer_suffix.as_deref().unwrap_or("none"),
        control.address_as.as_deref().unwrap_or("none"),
        control.quote_question,
        control.format_instruction.as_deref().unwrap_or("none"),
        control.completion_instruction.as_deref().unwrap_or("none")
    )
}

pub fn describe_goal(goal: &ConversationGoal, state: &GoalState) -> String {
    if !goal.is_enabled() {
        return "Conversation goal: none".to_string();
    }
    format!(
        "Conversation goal: mode={}, required_fields={}, {}",
        goal.mode,
        goal.required_fields.join(", "),
        state.summary()
    )
}

fn parse_stop_mode(value: &str) -> Option<ConversationStopMode> {
    match value {
        "manual" => Some(ConversationStopMode::Manual),
        "state" => Some(ConversationStopMode::State),
        "instruction" => Some(ConversationStopMode::Instruction),
        "combined" => Some(ConversationStopMode::Combined),
        _ => None,
    }
}

fn parse_answer_format(value: &str) -> Option<AnswerFormat> {
    match value {
        "natural" => Some(AnswerFormat::Natural),
        "bullets" => Some(AnswerFormat::Bullets),
        "numbered" => Some(AnswerFormat::Numbered),
        "short" => Some(AnswerFormat::Short),
        "steps" => Some(AnswerFormat::Steps),
        "table" => Some(AnswerFormat::Table),
        _ => None,
    }
}

fn goal_format_instruction(required_fields: &[String]) -> String {
    format!(
        "You are collecting a structured entity. Return only a JSON object with this shape: {{\"fields\":{{{}}},\"next_question\":\"string or null\",\"done\":boolean}}. Use null for unknown fields.",
        required_fields
            .iter()
            .map(|field| format!("\"{field}\":null"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn goal_completion_instruction(required_fields: &[String]) -> String {
    format!(
        "When all required fields are known ({}), set done=true and stop asking follow-up questions. Otherwise set done=false and ask one next_question.",
        required_fields.join(", ")
    )
}

fn merge_instruction(existing: Option<String>, added: String) -> String {
    match existing {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}\n{added}"),
        _ => added,
    }
}

fn is_filled(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

pub fn save_history(profile_name: &str, messages: &[ChatMessage]) -> Result<PathBuf, AppError> {
    let dirs = ProjectDirs::from("dev", "ai-playground", "ai-playground").ok_or_else(|| {
        AppError::Config {
            path: PathBuf::from("<unknown>"),
            message: "Could not resolve data directory".to_string(),
        }
    })?;
    let dir = dirs.data_local_dir().join("history");
    fs::create_dir_all(&dir).map_err(|error| AppError::Config {
        path: dir.clone(),
        message: format!("could not create history directory: {error}"),
    })?;
    let filename = format!(
        "{}-{}.json",
        profile_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::Config {
                path: dir.clone(),
                message: error.to_string(),
            })?
            .as_secs()
    );
    let path = dir.join(filename);
    let raw = serde_json::to_string_pretty(messages)
        .map_err(|error| AppError::Json(error.to_string()))?;
    fs::write(&path, raw).map_err(|error| AppError::Config {
        path: path.clone(),
        message: format!("could not write history: {error}"),
    })?;
    Ok(path)
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

    #[test]
    fn goal_control_forces_json_and_preserves_user_instruction() {
        let goal = ConversationGoal {
            required_fields: vec!["topic".to_string()],
            mode: ConversationStopMode::Combined,
        };
        let control = goal.apply_to_control(ResponseControl {
            format_instruction: Some("Use concise values.".to_string()),
            ..ResponseControl::uncontrolled()
        });

        assert_eq!(control.format, ResponseFormat::JsonObject);
        assert!(
            control
                .format_instruction
                .expect("format instruction")
                .contains("Use concise values.")
        );
        assert!(
            control
                .completion_instruction
                .expect("completion instruction")
                .contains("done=true")
        );
    }

    #[test]
    fn terminal_line_decode_tolerates_invalid_utf8() {
        let decoded = decode_terminal_line(&[0xd0, 0x97, 0xff, 0xd0, 0x90]);

        assert!(decoded.contains('З'));
        assert!(decoded.contains('А'));
    }
}
