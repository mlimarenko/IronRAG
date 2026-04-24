// CRAG rewrite path + RetrievalConfidence live here but their call sites
// in `structured_query_pipeline.rs` are not wired into the v0.3.2
// retrieval pipeline. Marking the module-level allow so the file stays
// as the canonical home when the structured-query pipeline gets
// re-enabled in a later release.
#![allow(dead_code)]

use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ai::AiBindingPurpose, integrations::llm::ChatRequestSeed,
};

use super::types::RuntimeMatchedChunk;
use super::{
    CRAG_CONFIDENCE_THRESHOLD, CRAG_REWRITE_TEMPERATURE, CRAG_REWRITE_TIMEOUT, HYDE_TEMPERATURE,
    HYDE_TIMEOUT,
};

pub(super) struct RetrievalConfidence {
    pub(super) score: f32,
    pub(super) is_sufficient: bool,
}

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

pub(super) fn evaluate_retrieval_quality(
    chunks: &[RuntimeMatchedChunk],
    question_keywords: &[String],
) -> RetrievalConfidence {
    if chunks.is_empty() {
        return RetrievalConfidence { score: 0.0, is_sufficient: false };
    }

    let top_n = chunks.len().min(5);
    let top_chunks = &chunks[..top_n];

    let avg_score: f32 =
        top_chunks.iter().filter_map(|chunk| chunk.score).sum::<f32>() / top_n as f32;

    let keyword_coverage = if question_keywords.is_empty() {
        1.0_f32
    } else {
        let covered = question_keywords
            .iter()
            .filter(|keyword| {
                let lower_keyword = keyword.to_ascii_lowercase();
                top_chunks
                    .iter()
                    .any(|chunk| chunk.source_text.to_ascii_lowercase().contains(&lower_keyword))
            })
            .count();
        covered as f32 / question_keywords.len() as f32
    };

    let combined = 0.5 * avg_score + 0.5 * keyword_coverage;
    RetrievalConfidence { score: combined, is_sufficient: combined >= CRAG_CONFIDENCE_THRESHOLD }
}

pub(super) async fn rewrite_query_for_retry(
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
        "The following question did not find relevant documents. Rephrase it using different \
         terminology, synonyms, and more general terms:\n\n{question}\n\nRewritten question:"
    );

    let request = crate::integrations::llm::build_text_chat_request(
        ChatRequestSeed {
            provider_kind: binding.provider_kind,
            model_name: binding.model_name,
            api_key_override: binding.api_key,
            base_url_override: binding.provider_base_url,
            system_prompt: None,
            temperature: Some(CRAG_REWRITE_TEMPERATURE),
            top_p: None,
            max_output_tokens_override: Some(100),
            extra_parameters_json: serde_json::json!({}),
        },
        prompt,
    );

    let response = tokio::time::timeout(CRAG_REWRITE_TIMEOUT, state.llm_gateway.generate(request))
        .await
        .ok()?
        .ok()?;

    let rewritten = response.output_text.trim().to_string();
    if rewritten.is_empty() { None } else { Some(rewritten) }
}
