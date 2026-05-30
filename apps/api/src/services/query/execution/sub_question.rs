use uuid::Uuid;

use crate::{app::state::AppState, domains::ai::AiBindingPurpose};

use super::{HYDE_TEMPERATURE, HYDE_TIMEOUT};

/// Maximum number of sub-questions we ask the model to produce. Keeping
/// this small caps the fan-out cost: each sub-question becomes an
/// independent retrieve pass downstream.
const MAX_SUB_QUESTIONS: usize = 3;

/// One decomposed slice of an original query. The text is meant to be fed
/// back into the canonical retrieve+answer pipeline as if the user had asked
/// it directly. `rationale` records why the splitter chose this slice; it is
/// for tracing only and never reaches the user.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) struct SubQuestion {
    pub text: String,
    pub rationale: Option<String>,
}

/// Decompose a multi-fact question into a small set of independently
/// answerable sub-questions using the configured `QueryCompile` binding.
/// Returns an empty `Vec` when the model cannot find a clean split — the
/// caller should treat that as "answer the original question directly".
///
/// This is purely a query-planning helper. It does not run retrieve, it
/// does not synthesize an answer, it does not check feasibility. The
/// downstream `answer_pipeline` is responsible for fanning the sub-questions
/// out, running each through the canonical retrieve+answer pipeline, and
/// folding the partial answers back into a final response.
#[allow(dead_code)]
pub(super) async fn decompose_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
) -> anyhow::Result<Vec<SubQuestion>> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryCompile)
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to resolve QueryCompile binding for sub-questions: {error}")
        })?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=query_compile, reason=not_configured, library_id={library_id}")
        })?;

    let prompt = format!(
        "Decompose the question below into AT MOST {MAX_SUB_QUESTIONS} short, \
         independent sub-questions whose answers, combined, fully cover the \
         original. Each sub-question must be answerable on its own from the \
         same corpus. Preserve every named entity, code identifier, version \
         number, and quoted literal exactly as written. \
         Emit one sub-question per line. Do not number them. Do not add \
         commentary. If the question is already a single fact and cannot be \
         meaningfully split, emit nothing.\n\n\
         Question: {question}"
    );

    let mut seed = binding.chat_request_seed();
    seed.system_prompt = None;
    seed.temperature = Some(HYDE_TEMPERATURE);
    seed.top_p = None;
    seed.max_output_tokens_override = Some(300);
    let request = crate::integrations::llm::build_text_chat_request(seed, prompt);

    let response =
        match tokio::time::timeout(HYDE_TIMEOUT, state.llm_gateway.generate(request)).await {
            Ok(result) => {
                result.map_err(|error| anyhow::anyhow!("sub-question LLM call failed: {error}"))?
            }
            Err(_elapsed) => {
                return Err(anyhow::anyhow!(
                    "sub-question LLM call timed out after {} ms",
                    HYDE_TIMEOUT.as_millis()
                ));
            }
        };

    Ok(normalize_sub_questions(&response.output_text, question))
}

fn normalize_sub_questions(output_text: &str, original_question: &str) -> Vec<SubQuestion> {
    let normalized_original = original_question.trim().to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(MAX_SUB_QUESTIONS);
    for raw in output_text.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '•']).trim();
        if line.is_empty() {
            continue;
        }
        let key = line.to_lowercase();
        if key == normalized_original || !seen.insert(key) {
            continue;
        }
        out.push(SubQuestion { text: line.to_string(), rationale: None });
        if out.len() >= MAX_SUB_QUESTIONS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_sub_questions_strips_bullets_and_blanks() {
        let raw = "- What is the latest 1.4 release?\n\n* Which providers does 1.4 support?\n";
        let out = normalize_sub_questions(raw, "Full multi-fact question");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "What is the latest 1.4 release?");
        assert_eq!(out[1].text, "Which providers does 1.4 support?");
    }

    #[test]
    fn normalize_sub_questions_caps_at_max() {
        let raw = "Q one\nQ two\nQ three\nQ four\nQ five\n";
        let out = normalize_sub_questions(raw, "Source");
        assert_eq!(out.len(), MAX_SUB_QUESTIONS);
    }

    #[test]
    fn normalize_sub_questions_drops_duplicate_of_original() {
        let raw = "Source question\nA real sub-question\n";
        let out = normalize_sub_questions(raw, "Source question");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "A real sub-question");
    }

    #[test]
    fn normalize_sub_questions_returns_empty_when_model_emits_nothing() {
        let out = normalize_sub_questions("   \n\n  ", "Single fact question");
        assert!(out.is_empty());
    }
}
