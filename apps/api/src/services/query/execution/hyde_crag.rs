use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ai::AiBindingPurpose, integrations::llm::ChatRequestSeed,
};

use super::{HYDE_TEMPERATURE, HYDE_TIMEOUT};

pub(super) async fn generate_hyde_passage(
    state: &AppState,
    library_id: Uuid,
    question: &str,
) -> Option<String> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
        .await
        .ok()
        .flatten()?;

    let prompt = format!(
        "Write a short factual passage (2-3 sentences) that would answer this question. \
         Do not mention the question itself, just write the answer as if from a document:\n\n\
         Question: {question}"
    );

    let request = crate::integrations::llm::build_text_chat_request(
        ChatRequestSeed {
            provider_kind: binding.provider_kind,
            model_name: binding.model_name,
            api_key_override: binding.api_key,
            base_url_override: binding.provider_base_url,
            system_prompt: None,
            temperature: Some(HYDE_TEMPERATURE),
            top_p: None,
            max_output_tokens_override: Some(200),
            extra_parameters_json: serde_json::json!({}),
        },
        prompt,
    );

    let response =
        tokio::time::timeout(HYDE_TIMEOUT, state.llm_gateway.generate(request)).await.ok()?.ok()?;

    let passage = response.output_text.trim().to_string();
    if passage.is_empty() { None } else { Some(passage) }
}
