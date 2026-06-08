use std::io::{self, BufRead, Write};

use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{
        AnswerFormat, BillingLookup, ModelPricing, ProviderClient, ResponseControl, ResponseFormat,
    },
    secrets::{SecretStore, get_config_profile_token},
};

use super::{
    ChatAgent, LocalSessionStore, format_request_metrics,
    goal::{ConversationGoal, ConversationStopMode, GoalState},
    history::save_history,
    session_key,
};

pub async fn interactive_chat(
    client: &dyn ProviderClient,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    profile_name: &str,
    profile: &ProfileConfig,
    mut control: ResponseControl,
    pricing: Option<ModelPricing>,
    billing: Option<BillingLookup>,
    mut goal: ConversationGoal,
) -> Result<(), AppError> {
    let token =
        get_config_profile_token(secrets, config, profile_name, profile)?.ok_or_else(|| {
            AppError::MissingToken {
                profile: profile_name.to_string(),
            }
        })?;
    let store = LocalSessionStore::new()?;
    let session_key = session_key(profile_name, &profile.model);
    let mut session = store.load_or_create_latest(&session_key)?;
    let mut agent = ChatAgent::new(
        profile.clone(),
        token,
        session.messages.clone(),
        control.clone(),
        pricing,
        billing,
    );
    let mut goal_state = GoalState::new(goal.required_fields.clone());
    println!(
        "Chat started with profile '{profile_name}', model '{}', session '{}'.",
        profile.model, session.id
    );
    if !agent.history().is_empty() {
        println!("Loaded {} local history messages.", agent.history().len());
    }
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
                agent.clear_history();
                session = store.create_session()?;
                store.save_session(&session_key, &session.id, agent.history())?;
                println!("History cleared. New session: {}", session.id);
                continue;
            }
            "/save" => {
                let path = save_history(profile_name, agent.history())?;
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
                Ok(v) => {
                    control.max_tokens = Some(v);
                    println!("Max tokens: {v}");
                }
                Err(_) => println!("Use /max-tokens <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/temperature ") {
            match value.parse::<f32>() {
                Ok(v) => {
                    control.temperature = Some(v);
                    println!("Temperature: {v}");
                }
                Err(_) => println!("Use /temperature <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/top-p ") {
            match value.parse::<f32>() {
                Ok(v) => {
                    control.top_p = Some(v);
                    println!("Top-p: {v}");
                }
                Err(_) => println!("Use /top-p <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/presence-penalty ") {
            match value.parse::<f32>() {
                Ok(v) => {
                    control.presence_penalty = Some(v);
                    println!("Presence penalty: {v}");
                }
                Err(_) => println!("Use /presence-penalty <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/frequency-penalty ") {
            match value.parse::<f32>() {
                Ok(v) => {
                    control.frequency_penalty = Some(v);
                    println!("Frequency penalty: {v}");
                }
                Err(_) => println!("Use /frequency-penalty <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/seed ") {
            match value.parse::<i64>() {
                Ok(v) => {
                    control.seed = Some(v);
                    println!("Seed: {v}");
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
            } else if goal.required_fields.iter().any(|f| f == value) {
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

        eprintln!("Waiting for provider response...");
        let effective_control = goal.apply_to_control(control.clone());
        agent.set_control(effective_control);
        let response = agent.respond(client, line.to_string()).await?;
        store.save_session(&session_key, &session.id, agent.history())?;
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
    }

    if config.active_profile.as_deref() != Some(profile_name) {
        println!("Session used non-active profile '{profile_name}'.");
    }
    Ok(())
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
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .temperature
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .top_p
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .presence_penalty
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .frequency_penalty
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        control
            .seed
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        stop,
        control.answer_prefix.as_deref().unwrap_or("none"),
        control.answer_suffix.as_deref().unwrap_or("none"),
        control.address_as.as_deref().unwrap_or("none"),
        control.quote_question,
        control.format_instruction.as_deref().unwrap_or("none"),
        control.completion_instruction.as_deref().unwrap_or("none"),
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

pub fn read_terminal_line() -> Result<Option<String>, AppError> {
    let mut bytes = Vec::new();
    let read = io::stdin()
        .lock()
        .read_until(b'\n', &mut bytes)
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    while bytes.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        bytes.pop();
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_line_decode_tolerates_invalid_utf8() {
        // Can't call read_terminal_line directly (needs stdin), but verify the lossy decode logic
        let bytes: &[u8] = &[0xd0, 0x97, 0xff, 0xd0, 0x90];
        let decoded = String::from_utf8_lossy(bytes).into_owned();
        assert!(decoded.contains('З'));
        assert!(decoded.contains('А'));
    }
}
