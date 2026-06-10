use serde::Serialize;

use crate::providers::ProviderKind;

#[derive(Debug, Serialize)]
pub(crate) struct ParameterConstraintView {
    pub(crate) id: &'static str,
    pub(crate) supported: bool,
    pub(crate) min: Option<f32>,
    pub(crate) max: Option<f32>,
    pub(crate) step: Option<f32>,
    pub(crate) note: &'static str,
}

pub(crate) fn parameter_constraints(provider: ProviderKind) -> Vec<ParameterConstraintView> {
    let mut constraints = openrouter_like_constraints();
    match provider {
        ProviderKind::OpenRouter => constraints,
        ProviderKind::OpenAiCompatible => {
            mark_unsupported(
                &mut constraints,
                &["maxTokens"],
                "OpenAI chat models use max_completion_tokens; max_tokens is converted server-side for compatibility.",
            );
            constraints
        }
        ProviderKind::DeepSeek => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxCompletionTokens",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "repetitionPenalty",
                    "n",
                    "store",
                    "parallelToolCalls",
                ],
                "DeepSeek docs: unsupported/deprecated; the API will ignore this parameter.",
            );
            constraints
        }
        ProviderKind::Kimi => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxTokens",
                    "temperature",
                    "topP",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "repetitionPenalty",
                    "n",
                    "store",
                    "parallelToolCalls",
                ],
                "Kimi current docs do not list this parameter for Chat Completion.",
            );
            constraints
        }
        ProviderKind::GigaChat => {
            mark_unsupported(
                &mut constraints,
                &[
                    "maxCompletionTokens",
                    "topK",
                    "minP",
                    "topA",
                    "presencePenalty",
                    "frequencyPenalty",
                    "store",
                    "parallelToolCalls",
                ],
                "GigaChat docs do not list this parameter for Chat Completion.",
            );
            set_constraint(
                &mut constraints,
                "temperature",
                Some(0.01),
                None,
                "GigaChat docs: temperature must be > 0; values above 2 can be too random.",
            );
            constraints
        }
    }
}

fn openrouter_like_constraints() -> Vec<ParameterConstraintView> {
    vec![
        constraint("maxTokens", true, Some(1.0), None, Some(1.0), ">= 1"),
        constraint(
            "maxCompletionTokens",
            true,
            Some(1.0),
            None,
            Some(1.0),
            ">= 1",
        ),
        constraint("temperature", true, Some(0.0), Some(2.0), Some(0.1), "0..2"),
        constraint("topP", true, Some(0.0), Some(1.0), Some(0.05), "0..1"),
        constraint("topK", true, Some(0.0), None, Some(1.0), ">= 0"),
        constraint("minP", true, Some(0.0), Some(1.0), Some(0.01), "0..1"),
        constraint("topA", true, Some(0.0), Some(1.0), Some(0.01), "0..1"),
        constraint(
            "presencePenalty",
            true,
            Some(-2.0),
            Some(2.0),
            Some(0.1),
            "-2..2",
        ),
        constraint(
            "frequencyPenalty",
            true,
            Some(-2.0),
            Some(2.0),
            Some(0.1),
            "-2..2",
        ),
        constraint(
            "repetitionPenalty",
            true,
            Some(0.0),
            Some(2.0),
            Some(0.05),
            "0..2",
        ),
        constraint(
            "topLogprobs",
            true,
            Some(0.0),
            Some(20.0),
            Some(1.0),
            "0..20",
        ),
        constraint("n", true, Some(1.0), None, Some(1.0), ">= 1"),
        constraint("includeReasoning", true, None, None, None, "boolean"),
        constraint("logprobs", true, None, None, None, "boolean"),
        constraint("store", true, None, None, None, "boolean"),
        constraint(
            "parallelToolCalls",
            true,
            None,
            None,
            None,
            "Only send when tools are specified; OpenAI-compatible APIs reject it without tools.",
        ),
    ]
}

fn constraint(
    id: &'static str,
    supported: bool,
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
    note: &'static str,
) -> ParameterConstraintView {
    ParameterConstraintView {
        id,
        supported,
        min,
        max,
        step,
        note,
    }
}

fn mark_unsupported(constraints: &mut [ParameterConstraintView], ids: &[&str], note: &'static str) {
    for constraint in constraints {
        if ids.contains(&constraint.id) {
            constraint.supported = false;
            constraint.note = note;
        }
    }
}

fn set_constraint(
    constraints: &mut [ParameterConstraintView],
    id: &str,
    min: Option<f32>,
    max: Option<f32>,
    note: &'static str,
) {
    if let Some(constraint) = constraints
        .iter_mut()
        .find(|constraint| constraint.id == id)
    {
        constraint.min = min;
        constraint.max = max;
        constraint.note = note;
    }
}
