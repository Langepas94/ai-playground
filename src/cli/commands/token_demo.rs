use crate::{
    chat::{ContextStatus, TokenCostEstimate, TokenGrowthReport, simulate_growth},
    cli::args::TokenDemoArgs,
    providers::ModelPricing,
};

pub fn run_token_demo(args: &TokenDemoArgs) {
    let pricing = ModelPricing {
        currency: args.price_currency.clone(),
        input_per_million: Some(args.input_price_per_million),
        output_per_million: args.output_price_per_million,
        cache_hit_input_per_million: None,
        cache_miss_input_per_million: None,
    };
    let short = simulate_growth(
        "Короткий диалог",
        args.context_limit,
        args.short_turns,
        args.user_tokens_per_turn,
        args.response_tokens_per_turn,
        Some(&pricing),
    );
    let long = simulate_growth(
        "Длинный диалог",
        args.context_limit,
        args.long_turns,
        args.user_tokens_per_turn,
        args.response_tokens_per_turn,
        Some(&pricing),
    );
    let overflow = simulate_growth(
        "Диалог выше лимита модели",
        args.context_limit,
        args.overflow_turns,
        args.user_tokens_per_turn,
        args.response_tokens_per_turn,
        Some(&pricing),
    );

    println!(
        "Token accounting demo\ncontext_limit={} input_price={:.4}/1M output_price={:.4}/1M {}\n",
        args.context_limit,
        args.input_price_per_million,
        args.output_price_per_million,
        args.price_currency
    );
    print_report(&short);
    print_report(&long);
    print_report(&overflow);
}

fn print_report(report: &TokenGrowthReport) {
    println!("## {}", report.name);
    println!(
        "turn | request | history | response | total | cumulative | cost | cumulative cost | status"
    );
    println!(
        "-----|---------|---------|----------|-------|------------|------|-----------------|-------"
    );
    for row in &report.rows {
        println!(
            "{} | {} | {} | {} | {} | {} | {} | {} | {}",
            row.turn,
            row.request_tokens,
            row.history_tokens,
            row.response_tokens,
            row.total_tokens,
            row.cumulative_tokens,
            format_cost(row.cost.as_ref()),
            format_cost(row.cumulative_cost.as_ref()),
            format_status(row.status)
        );
    }
    if let Some(breakage) = &report.breakage {
        println!("\nЧто ломается: {breakage}");
    } else if let Some(last) = report.rows.last() {
        let remaining = i64::from(report.context_limit) - i64::from(last.total_tokens);
        println!("\nИтог: помещается, запас контекста {remaining} токенов.");
    }
    println!();
}

fn format_cost(cost: Option<&TokenCostEstimate>) -> String {
    cost.map(|cost| format!("{:.6} {}", cost.amount, cost.currency))
        .unwrap_or_else(|| "-".to_string())
}

fn format_status(status: ContextStatus) -> &'static str {
    match status {
        ContextStatus::Fits => "fits",
        ContextStatus::NearLimit => "near-limit",
        ContextStatus::Overflow => "overflow",
    }
}
