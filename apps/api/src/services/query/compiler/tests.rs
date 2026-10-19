mod cache;
mod fallback_and_prompt;
mod provider_behavior;

use super::*;
use crate::integrations::llm::{
    ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, VisionRequest, VisionResponse,
};
use std::sync::Mutex;

/// Scripted responses, consumed in order. A compile that produces an unusable
/// QueryIR is retried, so a stub that can answer only once cannot express
/// either side of that: `repeating` keeps answering the same way (the model
/// fails systematically), while `scripted` hands out a different answer per
/// attempt (the model slipped once).
struct StubGateway {
    responses: Mutex<Vec<Result<ChatResponse, anyhow::Error>>>,
    repeat_last: bool,
    calls: Mutex<u32>,
    last_request: Mutex<Option<ChatRequest>>,
}

impl StubGateway {
    /// Answers exactly once; a second call is a test failure. Use where the
    /// point is that the gateway must not be reached again.
    fn new(output: Result<ChatResponse, anyhow::Error>) -> Self {
        Self {
            responses: Mutex::new(vec![output]),
            repeat_last: false,
            calls: Mutex::new(0),
            last_request: Mutex::new(None),
        }
    }

    /// Answers the same way on every attempt.
    fn repeating(output: Result<ChatResponse, anyhow::Error>) -> Self {
        Self {
            responses: Mutex::new(vec![output]),
            repeat_last: true,
            calls: Mutex::new(0),
            last_request: Mutex::new(None),
        }
    }

    /// Answers each attempt from the script, in order.
    fn scripted(outputs: Vec<Result<ChatResponse, anyhow::Error>>) -> Self {
        Self {
            responses: Mutex::new(outputs.into_iter().rev().collect()),
            repeat_last: false,
            calls: Mutex::new(0),
            last_request: Mutex::new(None),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().unwrap()
    }
}

#[async_trait]
impl LlmGateway for StubGateway {
    async fn generate(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        *self.last_request.lock().unwrap() = Some(request);
        *self.calls.lock().unwrap() += 1;
        let mut responses = self.responses.lock().unwrap();
        if self.repeat_last && responses.len() == 1 {
            match responses.first().expect("checked len") {
                Ok(response) => Ok(response.clone()),
                Err(error) => Err(anyhow::anyhow!("{error}")),
            }
        } else {
            responses.pop().expect("stub gateway called more times than it was scripted for")
        }
    }
    async fn embed(&self, _: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        unreachable!()
    }
    async fn embed_many(&self, _: EmbeddingBatchRequest) -> anyhow::Result<EmbeddingBatchResponse> {
        unreachable!()
    }
    async fn vision_extract(&self, _: VisionRequest) -> anyhow::Result<VisionResponse> {
        unreachable!()
    }
}

fn sample_binding() -> ResolvedRuntimeBinding {
    ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose: AiBindingPurpose::QueryCompile,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "openai".to_string(),
        provider_base_url: None,
        provider_api_style: "openai".to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some("test-key".to_string()),
        model_catalog_id: Uuid::now_v7(),
        model_name: "gpt-5.4-nano".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    }
}

fn chat_response_with(output_text: &str) -> ChatResponse {
    ChatResponse {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-nano".to_string(),
        output_text: output_text.to_string(),
        usage_json: json!({"prompt_tokens": 100, "completion_tokens": 40}),
    }
}

fn compiled_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Ru,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Procedure],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}
