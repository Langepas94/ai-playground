use crate::providers::{ChatMessage, ModelPricing, Role};

const TOKENS_PER_MESSAGE: u32 = 4;
const TOKENS_PER_REPLY_PRIMER: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextStatus {
    Fits,
    NearLimit,
    Overflow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenCostEstimate {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenEstimate {
    pub current_request_tokens: u32,
    pub history_tokens: u32,
    pub response_tokens: u32,
    pub total_tokens: u32,
    pub context_limit: u32,
    pub remaining_tokens: i64,
    pub status: ContextStatus,
    pub cost: Option<TokenCostEstimate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenGrowthRow {
    pub turn: usize,
    pub request_tokens: u32,
    pub history_tokens: u32,
    pub response_tokens: u32,
    pub total_tokens: u32,
    pub cumulative_tokens: u32,
    pub cost: Option<TokenCostEstimate>,
    pub cumulative_cost: Option<TokenCostEstimate>,
    pub status: ContextStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenGrowthReport {
    pub name: String,
    pub context_limit: u32,
    pub rows: Vec<TokenGrowthRow>,
    pub breakage: Option<String>,
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    let mut tokens = 0_u32;
    let mut in_ascii_word = false;
    // Non-ASCII letters/digits (Cyrillic, CJK, etc.) are tokenized at roughly
    // 2 characters per token by modern BPE tokenizers (cl100k). Counting each
    // such char as a separate token over-estimated Cyrillic text ~4x, which in
    // turn inflated estimated cost when a provider omitted usage in the stream.
    let mut non_ascii_alnum = 0_u32;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if !in_ascii_word {
                tokens = tokens.saturating_add(1);
                in_ascii_word = true;
            }
        } else if character.is_alphanumeric() {
            in_ascii_word = false;
            non_ascii_alnum = non_ascii_alnum.saturating_add(1);
        } else {
            in_ascii_word = false;
            if character.is_whitespace() {
                continue;
            }
            tokens = tokens.saturating_add(1);
        }
    }
    tokens = tokens.saturating_add(non_ascii_alnum.div_ceil(2));
    tokens.max((text.chars().count() as u32).saturating_add(3) / 4)
}

pub fn estimate_message_tokens(message: &ChatMessage) -> u32 {
    TOKENS_PER_MESSAGE
        .saturating_add(role_tokens(&message.role))
        .saturating_add(estimate_text_tokens(&message.content))
}

pub fn estimate_messages_tokens(messages: &[ChatMessage]) -> u32 {
    messages
        .iter()
        .map(estimate_message_tokens)
        .sum::<u32>()
        .saturating_add(TOKENS_PER_REPLY_PRIMER)
}

pub fn estimate_exchange(
    request_messages: &[ChatMessage],
    history_messages: &[ChatMessage],
    response_text: &str,
    context_limit: u32,
    pricing: Option<&ModelPricing>,
) -> TokenEstimate {
    let current_request_tokens = estimate_messages_tokens(request_messages);
    let history_tokens = estimate_messages_tokens(history_messages);
    let response_tokens = estimate_text_tokens(response_text);
    let total_tokens = current_request_tokens.saturating_add(response_tokens);
    let remaining_tokens = i64::from(context_limit) - i64::from(total_tokens);
    let status = context_status(total_tokens, context_limit);
    TokenEstimate {
        current_request_tokens,
        history_tokens,
        response_tokens,
        total_tokens,
        context_limit,
        remaining_tokens,
        status,
        cost: pricing
            .map(|pricing| estimate_cost(current_request_tokens, response_tokens, pricing)),
    }
}

pub fn estimate_cost(
    input_tokens: u32,
    output_tokens: u32,
    pricing: &ModelPricing,
) -> TokenCostEstimate {
    let input_cost =
        f64::from(input_tokens) * pricing.input_per_million.unwrap_or_default() / 1_000_000.0;
    let output_cost = f64::from(output_tokens) * pricing.output_per_million / 1_000_000.0;
    TokenCostEstimate {
        amount: input_cost + output_cost,
        currency: pricing.currency.clone(),
    }
}

pub fn context_status(total_tokens: u32, context_limit: u32) -> ContextStatus {
    if total_tokens > context_limit {
        ContextStatus::Overflow
    } else if total_tokens.saturating_mul(100) >= context_limit.saturating_mul(85) {
        ContextStatus::NearLimit
    } else {
        ContextStatus::Fits
    }
}

pub fn simulate_growth(
    name: &str,
    context_limit: u32,
    turn_count: usize,
    user_tokens_per_turn: u32,
    response_tokens_per_turn: u32,
    pricing: Option<&ModelPricing>,
) -> TokenGrowthReport {
    let mut history = vec![ChatMessage {
        role: Role::System,
        content: repeated_words("system", 32),
    }];
    let mut rows = Vec::new();
    let mut cumulative_tokens = 0_u32;
    let mut cumulative_cost = pricing.map(|pricing| TokenCostEstimate {
        amount: 0.0,
        currency: pricing.currency.clone(),
    });
    let mut breakage = None;

    for turn in 1..=turn_count {
        let user = ChatMessage {
            role: Role::User,
            content: repeated_words("user", user_tokens_per_turn),
        };
        let assistant_text = repeated_words("answer", response_tokens_per_turn);
        let mut request_messages = history.clone();
        request_messages.push(user.clone());
        let estimate = estimate_exchange(
            &request_messages,
            &history,
            &assistant_text,
            context_limit,
            pricing,
        );
        cumulative_tokens = cumulative_tokens.saturating_add(estimate.total_tokens);
        if let (Some(total), Some(cost)) = (&mut cumulative_cost, &estimate.cost) {
            total.amount += cost.amount;
        }
        rows.push(TokenGrowthRow {
            turn,
            request_tokens: estimate.current_request_tokens,
            history_tokens: estimate.history_tokens,
            response_tokens: estimate.response_tokens,
            total_tokens: estimate.total_tokens,
            cumulative_tokens,
            cost: estimate.cost.clone(),
            cumulative_cost: cumulative_cost.clone(),
            status: estimate.status,
        });
        if estimate.status == ContextStatus::Overflow {
            breakage = Some(format!(
                "turn {turn}: request+response needs {} tokens, context limit is {context_limit}; provider would reject it or the agent must summarize/truncate history before retrying",
                estimate.total_tokens
            ));
            break;
        }
        history.push(user);
        history.push(ChatMessage {
            role: Role::Assistant,
            content: assistant_text,
        });
    }

    TokenGrowthReport {
        name: name.to_string(),
        context_limit,
        rows,
        breakage,
    }
}

fn role_tokens(role: &Role) -> u32 {
    match role {
        Role::System => 1,
        Role::User => 1,
        Role::Assistant => 1,
    }
}

fn repeated_words(word: &str, count: u32) -> String {
    (0..count)
        .map(|index| format!("{word}{index}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> ModelPricing {
        ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(1.0),
            output_per_million: 4.0,
            cache_hit_input_per_million: None,
            cache_miss_input_per_million: None,
        }
    }

    #[test]
    fn cyrillic_text_is_not_over_estimated() {
        // Regression: each Cyrillic char was counted as 2 tokens, inflating
        // estimated usage (and therefore cost) ~4x when a provider omitted usage.
        // Cyrillic tokenizes at roughly 2 chars/token, so a 7-letter word must
        // stay well under the old 14-token estimate.
        let word = "бюджеты"; // 7 Cyrillic letters
        let tokens = estimate_text_tokens(word);
        assert!(
            tokens <= 5,
            "expected <=5 tokens for a 7-letter Cyrillic word, got {tokens}"
        );

        // A longer Russian sentence must not blow up either.
        let sentence = "Расскажи мне новый анекдот про двух соседей пожалуйста";
        let sentence_tokens = estimate_text_tokens(sentence);
        let char_count = sentence.chars().count() as u32;
        assert!(
            sentence_tokens < char_count,
            "estimate {sentence_tokens} should be below char count {char_count}"
        );
    }

    #[test]
    fn token_estimate_splits_request_history_and_response() {
        let history = vec![ChatMessage {
            role: Role::System,
            content: "Answer briefly".to_string(),
        }];
        let mut request = history.clone();
        request.push(ChatMessage {
            role: Role::User,
            content: "Hello there".to_string(),
        });

        let estimate = estimate_exchange(&request, &history, "Hi", 128, Some(&pricing()));

        assert!(estimate.current_request_tokens > estimate.history_tokens);
        assert!(estimate.response_tokens > 0);
        assert_eq!(
            estimate.total_tokens,
            estimate.current_request_tokens + estimate.response_tokens
        );
        assert_eq!(estimate.status, ContextStatus::Fits);
        assert!(estimate.cost.expect("cost").amount > 0.0);
    }

    #[test]
    fn growth_report_shows_long_dialogue_cost_growth() {
        let short = simulate_growth("short", 4_096, 2, 20, 30, Some(&pricing()));
        let long = simulate_growth("long", 4_096, 8, 20, 30, Some(&pricing()));

        assert!(
            long.rows.last().unwrap().request_tokens > short.rows.last().unwrap().request_tokens
        );
        assert!(
            long.rows
                .last()
                .unwrap()
                .cumulative_cost
                .as_ref()
                .unwrap()
                .amount
                > short
                    .rows
                    .last()
                    .unwrap()
                    .cumulative_cost
                    .as_ref()
                    .unwrap()
                    .amount
        );
    }

    #[test]
    fn growth_report_marks_context_overflow() {
        let report = simulate_growth("overflow", 300, 20, 40, 40, None);

        assert_eq!(report.rows.last().unwrap().status, ContextStatus::Overflow);
        assert!(report.breakage.unwrap().contains("provider would reject"));
    }
}
