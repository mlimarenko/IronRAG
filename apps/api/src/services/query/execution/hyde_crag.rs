use uuid::Uuid;

use crate::{app::state::AppState, domains::ai::AiBindingPurpose};

use super::{HYDE_TEMPERATURE, HYDE_TIMEOUT};

pub(super) async fn generate_hyde_passage(
    state: &AppState,
    library_id: Uuid,
    question: &str,
) -> anyhow::Result<String> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryCompile)
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to resolve QueryCompile binding for HyDE: {error}")
        })?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=query_compile, reason=not_configured, library_id={library_id}")
        })?;

    let prompt = format!(
        "Write a short factual passage (2-3 sentences) that would answer this question. \
         Do not mention the question itself, just write the answer as if from a document:\n\n\
         Question: {question}"
    );

    let mut seed = binding.chat_request_seed();
    seed.system_prompt = None;
    seed.temperature = Some(HYDE_TEMPERATURE);
    seed.top_p = None;
    seed.max_output_tokens_override = Some(200);
    let request = crate::integrations::llm::build_text_chat_request(seed, prompt);

    let response = match tokio::time::timeout(HYDE_TIMEOUT, state.llm_gateway.generate(request))
        .await
    {
        Ok(result) => result.map_err(|error| anyhow::anyhow!("HyDE LLM call failed: {error}"))?,
        Err(_elapsed) => {
            return Err(anyhow::anyhow!(
                "HyDE LLM call timed out after {} ms",
                HYDE_TIMEOUT.as_millis()
            ));
        }
    };

    normalize_hyde_passage(&response.output_text)
}

fn normalize_hyde_passage(output_text: &str) -> anyhow::Result<String> {
    let passage = output_text.trim().to_string();
    if passage.is_empty() {
        return Err(anyhow::anyhow!("HyDE LLM call returned empty output"));
    }
    Ok(passage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_hyde_passage_rejects_empty_output() {
        let error = normalize_hyde_passage(" \n\t ").expect_err("empty HyDE output must fail loud");

        assert!(error.to_string().contains("empty output"), "unexpected error: {error:#}");
    }

    #[test]
    fn normalize_hyde_passage_trims_non_empty_output() {
        let passage = normalize_hyde_passage("\n Short factual passage. \t").unwrap();

        assert_eq!(passage, "Short factual passage.");
    }
}
