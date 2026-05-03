use anyhow::Context;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    integrations::llm::{ChatRequestSeed, build_text_chat_request},
};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExpandedQuery {
    pub(crate) variants: Vec<String>, // cap 4 (original prepended after parse)
    pub(crate) high_keywords: Vec<String>, // cap 8
    pub(crate) low_keywords: Vec<String>, // cap 8
}

const VARIANT_CAP: usize = 4;
const KEYWORD_CAP: usize = 8;

/// Calls the Utility LLM binding to decompose `question` into alternative
/// phrasings and keyword tiers. The original question is always prepended as
/// `variants[0]` so callers can iterate `variants` without special-casing it.
///
/// Fail-loud: a JSON parse failure or a missing Utility binding propagates as
/// an error to the caller — no silent fallback to single-query.
pub(crate) async fn expand_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
) -> anyhow::Result<ExpandedQuery> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Utility)
        .await
        .context("failed to resolve Utility binding for query expansion")?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=utility, reason=not_configured, library_id={library_id}")
        })?;

    let prompt = format!(
        "Decompose the following user question into JSON with three fields:\n\
         - variants: 3 alternative phrasings that retrieve the same answer with different vocabulary\n\
         - high_keywords: high-level theme keywords (broad concepts), at most 8\n\
         - low_keywords: low-level specific keywords (named entities, parameters, paths, version numbers), at most 8\n\
         \n\
         Return ONLY strict JSON, no commentary:\n\
         {{\"variants\": [\"...\",\"...\",\"...\"], \"high_keywords\": [...], \"low_keywords\": [...]}}\n\
         \n\
         Question: {question}"
    );

    let request = build_text_chat_request(
        ChatRequestSeed {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            system_prompt: binding.system_prompt.clone(),
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: Some(binding.max_output_tokens_override.unwrap_or(400)),
            extra_parameters_json: binding.extra_parameters_json.clone(),
        },
        prompt,
    );

    let started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate(request)
        .await
        .context("Utility LLM call for query expansion failed")?;
    let expansion_ms = started.elapsed().as_millis();

    let mut parsed: ExpandedQuery = serde_json::from_str(response.output_text.trim())
        .with_context(|| {
            format!("failed to parse expand_query JSON: {:?}", response.output_text)
        })?;

    // Apply caps and prepend original question as variants[0].
    parsed.variants.truncate(VARIANT_CAP - 1);
    let mut variants = Vec::with_capacity(VARIANT_CAP);
    variants.push(question.to_string());
    variants.extend(parsed.variants);
    parsed.variants = variants;
    parsed.high_keywords.truncate(KEYWORD_CAP);
    parsed.low_keywords.truncate(KEYWORD_CAP);

    tracing::info!(
        stage = "query.expand",
        library_id = %library_id,
        variants_count = parsed.variants.len(),
        high_keywords_count = parsed.high_keywords.len(),
        low_keywords_count = parsed.low_keywords.len(),
        expansion_ms,
        "query expansion complete"
    );

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct StubGateway {
        output: Mutex<Option<anyhow::Result<ChatResponse>>>,
    }

    impl StubGateway {
        fn err(msg: &str) -> Self {
            Self { output: Mutex::new(Some(Err(anyhow::anyhow!("{}", msg)))) }
        }
    }

    #[async_trait]
    impl LlmGateway for StubGateway {
        async fn generate(&self, _request: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.output.lock().unwrap().take().expect("stub called twice")
        }
        async fn embed(&self, _: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
            unreachable!()
        }
        async fn embed_many(
            &self,
            _: EmbeddingBatchRequest,
        ) -> anyhow::Result<EmbeddingBatchResponse> {
            unreachable!()
        }
        async fn vision_extract(&self, _: VisionRequest) -> anyhow::Result<VisionResponse> {
            unreachable!()
        }
    }

    // Helper: parse raw JSON via the same post-processing as expand_query.
    fn apply_caps(json: &str, question: &str) -> ExpandedQuery {
        let mut parsed: ExpandedQuery = serde_json::from_str(json).unwrap();
        parsed.variants.truncate(VARIANT_CAP - 1);
        let mut variants = Vec::with_capacity(VARIANT_CAP);
        variants.push(question.to_string());
        variants.extend(parsed.variants);
        parsed.variants = variants;
        parsed.high_keywords.truncate(KEYWORD_CAP);
        parsed.low_keywords.truncate(KEYWORD_CAP);
        parsed
    }

    #[test]
    fn cap_enforcement_when_llm_returns_excess_variants() {
        // LLM returns 6 variants — after cap, we should keep VARIANT_CAP total
        // (original + at most 3 from the model).
        let json = r#"{
            "variants": ["v1","v2","v3","v4","v5","v6"],
            "high_keywords": ["kh1","kh2","kh3","kh4","kh5","kh6","kh7","kh8","kh9","kh10"],
            "low_keywords": ["kl1","kl2","kl3","kl4","kl5","kl6","kl7","kl8","kl9","kl10"]
        }"#;
        let result = apply_caps(json, "original question");

        assert_eq!(result.variants.len(), VARIANT_CAP);
        assert_eq!(result.variants[0], "original question");
        // After truncating to VARIANT_CAP - 1 = 3, variants[1..] are v1, v2, v3.
        assert_eq!(result.variants[1], "v1");
        assert_eq!(result.variants[2], "v2");
        assert_eq!(result.variants[3], "v3");

        assert_eq!(result.high_keywords.len(), KEYWORD_CAP);
        assert_eq!(result.low_keywords.len(), KEYWORD_CAP);
    }

    #[test]
    fn original_question_always_prepended_as_first_variant() {
        let json = r#"{
            "variants": ["alternative phrasing"],
            "high_keywords": ["concept"],
            "low_keywords": ["entity"]
        }"#;
        let result = apply_caps(json, "my original question");

        assert_eq!(result.variants[0], "my original question");
        assert_eq!(result.variants[1], "alternative phrasing");
    }

    #[test]
    fn json_parse_failure_is_captured_by_serde() {
        // Verify that invalid JSON is rejected by serde_json (mirrors the
        // expand_query fail-loud contract without needing AppState).
        let bad = "not json at all";
        let result: Result<ExpandedQuery, _> = serde_json::from_str(bad);
        assert!(result.is_err(), "expected parse error on invalid JSON");
    }

    #[test]
    fn partial_missing_field_is_parse_error() {
        // JSON missing low_keywords should be a serde error (all three fields are required).
        let bad = r#"{"variants": ["v1"], "high_keywords": ["kh1"]}"#;
        let result: Result<ExpandedQuery, _> = serde_json::from_str(bad);
        assert!(result.is_err(), "expected serde error on missing low_keywords field");
    }

    // Test that StubGateway::err propagates (gateway-level failure path).
    // This doesn't require AppState — just validates the stub itself.
    #[tokio::test]
    async fn stub_gateway_error_propagates() {
        use crate::integrations::llm::ChatRequest;
        let stub = StubGateway::err("provider unavailable");
        let request = ChatRequest {
            provider_kind: "openai".to_string(),
            model_name: "gpt-5.4-nano".to_string(),
            prompt: "test".to_string(),
            api_key_override: None,
            base_url_override: None,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            response_format: None,
            extra_parameters_json: serde_json::json!({}),
        };
        let result = stub.generate(request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("provider unavailable"));
    }
}
