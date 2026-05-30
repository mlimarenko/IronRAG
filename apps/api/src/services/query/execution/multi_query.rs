use uuid::Uuid;

use crate::{app::state::AppState, domains::ai::AiBindingPurpose};

use super::{HYDE_TEMPERATURE, HYDE_TIMEOUT};

/// Maximum number of query paraphrases requested from the LLM.
/// The model is asked to emit one paraphrase per line; lines are trimmed,
/// deduplicated against the original question, and capped at this many.
pub(super) const MAX_QUERY_PARAPHRASES: usize = 2;

/// Asks the configured `QueryCompile` binding to rewrite a question into a
/// small set of paraphrases for multi-query expansion. The result is meant
/// to feed additional retrieve passes whose hits will be RRF-merged with the
/// original-question hits — this fixes the vocabulary mismatch where the
/// user's phrasing diverges from the corpus phrasing.
///
/// Returns an empty `Vec` if the LLM call times out or yields no usable
/// rewrites; the caller should treat empty as "no expansion" and fall back
/// to the original-question lane.
#[allow(dead_code)]
pub(super) async fn generate_query_paraphrases(
    state: &AppState,
    library_id: Uuid,
    question: &str,
) -> anyhow::Result<Vec<String>> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryCompile)
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to resolve QueryCompile binding for paraphrases: {error}")
        })?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=query_compile, reason=not_configured, library_id={library_id}")
        })?;

    let prompt = format!(
        "Produce {MAX_QUERY_PARAPHRASES} short paraphrases of the question below. \
         Preserve every named entity, code identifier, version number, and quoted literal exactly. \
         Each paraphrase must be a single line. Do not number them. Do not add commentary.\n\n\
         Question: {question}"
    );

    let mut seed = binding.chat_request_seed();
    seed.system_prompt = None;
    seed.temperature = Some(HYDE_TEMPERATURE);
    seed.top_p = None;
    seed.max_output_tokens_override = Some(200);
    let request = crate::integrations::llm::build_text_chat_request(seed, prompt);

    let response =
        match tokio::time::timeout(HYDE_TIMEOUT, state.llm_gateway.generate(request)).await {
            Ok(result) => {
                result.map_err(|error| anyhow::anyhow!("paraphrase LLM call failed: {error}"))?
            }
            Err(_elapsed) => {
                return Err(anyhow::anyhow!(
                    "paraphrase LLM call timed out after {} ms",
                    HYDE_TIMEOUT.as_millis()
                ));
            }
        };

    Ok(normalize_paraphrases(&response.output_text, question))
}

fn normalize_paraphrases(output_text: &str, original_question: &str) -> Vec<String> {
    let normalized_original = original_question.trim().to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(MAX_QUERY_PARAPHRASES);
    for raw in output_text.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '•']).trim();
        if line.is_empty() {
            continue;
        }
        let key = line.to_lowercase();
        if key == normalized_original || !seen.insert(key) {
            continue;
        }
        out.push(line.to_string());
        if out.len() >= MAX_QUERY_PARAPHRASES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paraphrases_strips_bullets_and_blanks() {
        let raw = "- First rewrite of the query\n  \n* Second different rewrite\n";
        let out = normalize_paraphrases(raw, "Original question");
        assert_eq!(out, vec!["First rewrite of the query", "Second different rewrite"]);
    }

    #[test]
    fn normalize_paraphrases_caps_at_max() {
        let raw = "Variant one\nVariant two\nVariant three\nVariant four\n";
        let out = normalize_paraphrases(raw, "Source");
        assert_eq!(out.len(), MAX_QUERY_PARAPHRASES);
        assert_eq!(out[0], "Variant one");
    }

    #[test]
    fn normalize_paraphrases_drops_duplicate_of_original() {
        let raw = "Original question\nA real paraphrase\n";
        let out = normalize_paraphrases(raw, "Original question");
        assert_eq!(out, vec!["A real paraphrase"]);
    }

    #[test]
    fn normalize_paraphrases_dedupes_repeated_lines() {
        let raw = "Same line\nSame line\nDifferent line\n";
        let out = normalize_paraphrases(raw, "Source");
        assert_eq!(out, vec!["Same line", "Different line"]);
    }
}
