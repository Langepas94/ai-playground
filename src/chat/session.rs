use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use crate::{
    errors::AppError,
    providers::{AnswerFormat, ResponseControl, ResponseFormat},
};

use super::{
    ChatAgent, ChatRuntime, LocalSessionStore, MemoryConfig, RequestOptions, SelectedProfile,
    format_request_metrics,
    goal::{ConversationGoal, ConversationStopMode, GoalState},
    history::save_history,
    memory::MemoryStrategy,
    session_key,
};

pub async fn interactive_chat(
    runtime: ChatRuntime<'_>,
    profile: SelectedProfile<'_>,
    mut control: ResponseControl,
    options: RequestOptions,
    mut goal: ConversationGoal,
    mut memory_config: MemoryConfig,
) -> Result<(), AppError> {
    let token = crate::chat::resolve_profile_token(runtime, profile)?;
    let store = LocalSessionStore::new()?;
    let session_key = session_key(profile.name, &profile.config.model);
    let mut session = store.load_or_create_latest(&session_key)?;
    let mut memory = store.load_memory(&session.id)?;
    let mut agent = ChatAgent::new(
        profile.config.clone(),
        token,
        session.messages.clone(),
        memory,
        control.clone(),
        options.pricing,
        options.billing,
    );
    agent.set_memory_config(memory_config.clone());
    agent.set_context_limit(options.context_limit);
    let mut goal_state = GoalState::new(goal.required_fields.clone());
    let mut pending_attachments: Vec<PathBuf> = Vec::new();
    println!(
        "Chat started with profile '{profile_name}', model '{}', session '{}'.",
        profile.config.model,
        session.id,
        profile_name = profile.name
    );
    if !agent.history().is_empty() {
        println!("Loaded {} local history messages.", agent.history().len());
    }
    println!(
        "Use /exit, /profile, /model, /clear, /save, /control, /memory, /format, /answer-format, /max-tokens, /temperature, /top-p, /presence-penalty, /frequency-penalty, /seed, /stop, /goal, /attach <path>."
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
                println!("{}", profile.name);
                continue;
            }
            "/model" => {
                println!("{}", profile.config.model);
                continue;
            }
            "/clear" => {
                agent.clear_history();
                session = store.create_session()?;
                memory = agent.memory().clone();
                store.save_session(&session_key, &session.id, agent.history())?;
                store.save_memory(&session.id, &memory)?;
                println!("History cleared. New session: {}", session.id);
                continue;
            }
            "/save" => {
                let path = save_history(profile.name, agent.history())?;
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
            "/memory" => {
                println!("{}", describe_memory(&memory_config, agent.memory()));
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
            "/attach clear" => {
                pending_attachments.clear();
                println!("Pending attachments cleared.");
                continue;
            }
            "/attach" => {
                if pending_attachments.is_empty() {
                    println!("No pending attachments.");
                } else {
                    for p in &pending_attachments {
                        println!("  {}", p.display());
                    }
                }
                continue;
            }
            _ => {}
        }

        if let Some(path_str) = line.strip_prefix("/attach ") {
            let path = PathBuf::from(path_str.trim());
            if !path.exists() {
                println!("File not found: {}", path.display());
            } else {
                println!("Attached: {}", path.display());
                pending_attachments.push(path);
            }
            continue;
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
                "toon" => {
                    control.format = ResponseFormat::Toon;
                    println!("Response format: toon");
                }
                _ => println!("Use /format text, /format json-object, or /format toon."),
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
        if let Some(value) = line.strip_prefix("/memory strategy ") {
            match parse_memory_strategy(value) {
                Some(strategy) => {
                    memory_config.strategy = strategy;
                    agent.set_memory_config(memory_config.clone());
                    println!("Memory strategy: {strategy}");
                }
                None => {
                    println!(
                        "Use /memory strategy summary|sliding-window|sticky-facts|branching|scoped-branches."
                    );
                }
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory recent ") {
            match value.parse::<usize>() {
                Ok(v) => {
                    memory_config.recent_messages = v;
                    agent.set_memory_config(memory_config.clone());
                    println!("Memory recent messages: {v}");
                }
                Err(_) => println!("Use /memory recent <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory after ") {
            match value.parse::<usize>() {
                Ok(v) => {
                    memory_config.summarize_after_messages = v;
                    agent.set_memory_config(memory_config.clone());
                    println!("Memory summarize after messages: {v}");
                }
                Err(_) => println!("Use /memory after <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory chunk ") {
            match value.parse::<usize>() {
                Ok(v) => {
                    memory_config.summary_chunk_messages = v;
                    agent.set_memory_config(memory_config.clone());
                    println!("Memory summary chunk messages: {v}");
                }
                Err(_) => println!("Use /memory chunk <number>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory percent ") {
            match value.parse::<u8>() {
                Ok(v) => {
                    memory_config.summarize_at_context_percent = v;
                    agent.set_memory_config(memory_config.clone());
                    println!("Memory summarize at context percent: {v}");
                }
                Err(_) => println!("Use /memory percent <0-100>."),
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory summary-prompt ") {
            let value = value.trim();
            if value.is_empty() {
                println!("Use /memory summary-prompt <prompt>.");
            } else {
                memory_config.summary_prompt = value.to_string();
                agent.set_memory_config(memory_config.clone());
                println!("Memory summary prompt updated.");
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory facts-prompt ") {
            let value = value.trim();
            if value.is_empty() {
                println!("Use /memory facts-prompt <prompt>.");
            } else {
                memory_config.facts_prompt = value.to_string();
                agent.set_memory_config(memory_config.clone());
                println!("Memory facts prompt updated.");
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory facts-extraction-prompt ") {
            let value = value.trim();
            if value.is_empty() {
                println!("Use /memory facts-extraction-prompt <prompt>.");
            } else {
                memory_config.facts_extraction_prompt = value.to_string();
                agent.set_memory_config(memory_config.clone());
                println!("Memory facts extraction prompt updated.");
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory branch ") {
            let value = value.trim();
            if value.is_empty() {
                println!("Use /memory branch <branch-name>.");
            } else {
                memory_config.active_branch = value.to_string();
                memory_config.scoped_auto_route = false;
                agent.set_memory_config(memory_config.clone());
                println!("Memory manual branch: {value}");
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("/memory scoped-auto ") {
            match value {
                "on" | "true" | "1" => {
                    memory_config.scoped_auto_route = true;
                    agent.set_memory_config(memory_config.clone());
                    println!("Scoped topic auto-routing: on");
                }
                "off" | "false" | "0" => {
                    memory_config.scoped_auto_route = false;
                    agent.set_memory_config(memory_config.clone());
                    println!("Scoped topic auto-routing: off");
                }
                _ => println!("Use /memory scoped-auto on|off."),
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
        let user_text = build_message_with_attachments(line, &pending_attachments)?;
        pending_attachments.clear();
        let effective_control = goal.apply_to_control(control.clone());
        agent.set_control(effective_control);
        let response = agent.respond(runtime.client, user_text).await?;
        memory = agent.memory().clone();
        store.save_session(&session_key, &session.id, agent.history())?;
        store.save_memory(&session.id, &memory)?;
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

    if runtime.config.active_profile.as_deref() != Some(profile.name) {
        println!("Session used non-active profile '{}'.", profile.name);
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

pub fn describe_memory(config: &MemoryConfig, memory: &super::AgentMemory) -> String {
    let summary = memory
        .session_summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|_| format!("summary=present@{}", memory.summarized_message_count))
        .unwrap_or_else(|| "summary=none".to_string());
    let facts = memory
        .facts
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Memory: strategy={}, recent_messages={}, summarize_after_messages={}, summary_chunk_messages={}, summarize_at_context_percent={}, summary_prompt={}, {}, active_branch={}, scoped_auto_route={}, facts_extraction_prompt={}, facts_prompt={}, facts={}",
        config.strategy,
        config.recent_messages,
        config.summarize_after_messages,
        config.summary_chunk_messages,
        config.summarize_at_context_percent,
        config.summary_prompt,
        summary,
        config.active_branch,
        config.scoped_auto_route,
        config.facts_extraction_prompt,
        config.facts_prompt,
        if facts.is_empty() { "none" } else { &facts }
    )
}

pub fn parse_memory_strategy(value: &str) -> Option<MemoryStrategy> {
    match value {
        "summary" => Some(MemoryStrategy::Summary),
        "sliding-window" | "sliding" => Some(MemoryStrategy::SlidingWindow),
        "sticky-facts" | "facts" => Some(MemoryStrategy::StickyFacts),
        "branching" | "branches" => Some(MemoryStrategy::Branching),
        "scoped-branches" | "scoped" => Some(MemoryStrategy::ScopedBranches),
        _ => None,
    }
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

fn build_message_with_attachments(
    message: &str,
    attachments: &[PathBuf],
) -> Result<String, AppError> {
    if attachments.is_empty() {
        return Ok(message.to_string());
    }
    let mut parts = vec![message.to_string()];
    for path in attachments {
        let content = std::fs::read_to_string(path).map_err(|error| {
            AppError::InvalidInput(format!("Cannot read file '{}': {error}", path.display()))
        })?;
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        parts.push(format!("--- {label} ---\n{content}"));
    }
    Ok(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn terminal_line_decode_tolerates_invalid_utf8() {
        // Can't call read_terminal_line directly (needs stdin), but verify the lossy decode logic
        let bytes: &[u8] = &[0xd0, 0x97, 0xff, 0xd0, 0x90];
        let decoded = String::from_utf8_lossy(bytes).into_owned();
        assert!(decoded.contains('З'));
        assert!(decoded.contains('А'));
    }

    #[test]
    fn memory_strategy_parser_accepts_public_strategy_names_and_aliases() {
        assert_eq!(
            parse_memory_strategy("sliding-window"),
            Some(MemoryStrategy::SlidingWindow)
        );
        assert_eq!(
            parse_memory_strategy("sliding"),
            Some(MemoryStrategy::SlidingWindow)
        );
        assert_eq!(
            parse_memory_strategy("sticky-facts"),
            Some(MemoryStrategy::StickyFacts)
        );
        assert_eq!(
            parse_memory_strategy("facts"),
            Some(MemoryStrategy::StickyFacts)
        );
        assert_eq!(
            parse_memory_strategy("branching"),
            Some(MemoryStrategy::Branching)
        );
        assert_eq!(
            parse_memory_strategy("branches"),
            Some(MemoryStrategy::Branching)
        );
        assert_eq!(
            parse_memory_strategy("scoped-branches"),
            Some(MemoryStrategy::ScopedBranches)
        );
        assert_eq!(
            parse_memory_strategy("scoped"),
            Some(MemoryStrategy::ScopedBranches)
        );
        assert_eq!(
            parse_memory_strategy("summary"),
            Some(MemoryStrategy::Summary)
        );
    }

    #[test]
    fn describe_memory_reports_summary_prompt_and_facts() {
        let mut facts = BTreeMap::new();
        facts.insert("goal".to_string(), "test context strategies".to_string());
        let memory = super::super::AgentMemory {
            facts,
            branch_assignments: Default::default(),
            session_summary: Some("legacy summary should stay hidden".to_string()),
            summarized_message_count: 42,
        };

        let description = describe_memory(
            &MemoryConfig {
                strategy: MemoryStrategy::StickyFacts,
                recent_messages: 3,
                summary_prompt: "Custom summary prompt".to_string(),
                facts_extraction_prompt: "Collect only constraints.".to_string(),
                facts_prompt: "Custom facts prompt".to_string(),
                active_branch: "alpha".to_string(),
                ..MemoryConfig::default()
            },
            &memory,
        );

        assert!(description.contains("strategy=sticky-facts"));
        assert!(description.contains("recent_messages=3"));
        assert!(description.contains("summary_prompt=Custom summary prompt"));
        assert!(description.contains("summary=present@42"));
        assert!(description.contains("active_branch=alpha"));
        assert!(description.contains("scoped_auto_route=true"));
        assert!(description.contains("facts_extraction_prompt=Collect only constraints."));
        assert!(description.contains("facts_prompt=Custom facts prompt"));
        assert!(description.contains("goal=test context strategies"));
    }
}
