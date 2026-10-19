use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::parse::{
    normalize_graph_extraction_output, normalized_graph_extraction_json,
    parse_graph_extraction_output, repair_graph_extraction_normalized_json,
    sanitize_graph_extraction_candidate_set,
};
use super::prompt::{
    GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES, build_graph_extraction_prompt,
    build_graph_extraction_prompt_plan, build_graph_extraction_prompt_preview,
    graph_extraction_response_format, graph_extraction_static_sections,
};
use super::session::{
    GraphExtractionLoopRequest, GraphExtractionRawOutput, aggregate_provider_usage_json,
    build_provider_usage_json, build_raw_output_json, resolve_graph_extraction_with_gateway,
};
use super::types::*;
use super::{
    GraphEntityTypeClassifierIdentity, build_graph_extraction_cache_fingerprint,
    graph_extraction_contract_hash, graph_extraction_contract_hash_with,
    graph_extraction_prompt_hash, graph_extraction_stable_context, update_hash_classifier_identity,
};
use crate::{
    agent_runtime::{
        default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules},
        executor::RuntimeExecutor,
        hooks::{NoopRuntimeHooks, RuntimeHooks},
        policy::RuntimePolicy,
        registry::RuntimeTaskRegistry,
        response::RuntimeTerminalOutcome,
        task::RuntimeTaskRequest,
        tasks::graph_extract::{GraphExtractTask, GraphExtractTaskInput},
    },
    domains::ai::AiBindingPurpose,
    domains::{
        agent_runtime::{RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState},
        runtime_graph::RuntimeNodeType,
        runtime_ingestion::RuntimeProviderFailureClass,
    },
    infra::repositories::{ChunkRow, DocumentRow},
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    integrations::retry::ProviderCallError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::extraction_recovery::ExtractionRecoveryService,
    },
    shared::extraction::technical_facts::TechnicalFactQualifier,
};

use super::recovery::{
    GraphRuntimeStageUpdate, make_graph_terminal_failure_outcome, record_graph_runtime_stage,
};

struct FakeGateway {
    responses: Mutex<Vec<Result<ChatResponse>>>,
}

#[async_trait]
impl LlmGateway for FakeGateway {
    async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
        self.responses.lock().expect("lock fake responses").remove(0)
    }

    async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        unreachable!("embed is not used in graph extraction tests")
    }

    async fn embed_many(&self, _request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        unreachable!("embed_many is not used in graph extraction tests")
    }

    async fn vision_extract(&self, _request: VisionRequest) -> Result<VisionResponse> {
        unreachable!("vision_extract is not used in graph extraction tests")
    }
}

fn sample_document() -> DocumentRow {
    DocumentRow {
        id: uuid::Uuid::nil(),
        library_id: uuid::Uuid::nil(),
        source_id: None,
        external_key: "spec.md".to_string(),
        title: Some("Spec".to_string()),
        mime_type: Some("text/markdown".to_string()),
        checksum: None,
        active_revision_id: None,
        document_state: "active".to_string(),
        mutation_kind: None,
        mutation_status: None,
        deleted_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn sample_chunk() -> ChunkRow {
    ChunkRow {
        id: uuid::Uuid::nil(),
        document_id: uuid::Uuid::nil(),
        library_id: uuid::Uuid::nil(),
        ordinal: 0,
        content: "Provider Alpha supplies embeddings for the annual report graph.".to_string(),
        token_count: None,
        metadata_json: serde_json::json!({}),
        created_at: chrono::Utc::now(),
    }
}

fn sample_runtime_binding() -> ResolvedRuntimeBinding {
    ResolvedRuntimeBinding {
        binding_id: uuid::Uuid::now_v7(),
        workspace_id: uuid::Uuid::nil(),
        library_id: uuid::Uuid::nil(),
        binding_purpose: AiBindingPurpose::ExtractGraph,
        provider_catalog_id: uuid::Uuid::now_v7(),
        provider_kind: "provider-alpha".to_string(),
        provider_base_url: None,
        provider_api_style: "provider-alpha-compatible".to_string(),
        account_id: uuid::Uuid::now_v7(),
        api_key: Some("test-api-key".to_string()),
        model_catalog_id: uuid::Uuid::now_v7(),
        model_name: "alpha-chat-mini".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    }
}

fn sample_request() -> GraphExtractionRequest {
    GraphExtractionRequest {
        library_id: uuid::Uuid::nil(),
        document: sample_document(),
        chunk: sample_chunk(),
        structured_chunk: GraphExtractionStructuredChunkContext {
            chunk_kind: Some("endpoint_block".to_string()),
            section_path: vec!["REST API".to_string(), "Status".to_string()],
            heading_trail: vec!["REST API".to_string()],
            support_block_ids: vec![uuid::Uuid::now_v7()],
            literal_digest: Some("digest".to_string()),
        },
        technical_facts: vec![
            GraphExtractionTechnicalFact {
                fact_kind: "http_method".to_string(),
                normalized_value: "GET".to_string(),
                display_value: "GET".to_string(),
                qualifiers: Vec::new(),
            },
            GraphExtractionTechnicalFact {
                fact_kind: "endpoint_path".to_string(),
                normalized_value: "/annual-report/graph".to_string(),
                display_value: "/annual-report/graph".to_string(),
                qualifiers: vec![TechnicalFactQualifier {
                    key: "method".to_string(),
                    value: "GET".to_string(),
                }],
            },
        ],
        revision_id: None,
        activated_by_attempt_id: None,
        resume_hint: None,
        library_extraction_prompt: None,
        sub_type_hints: GraphExtractionSubTypeHints::default(),
    }
}

fn oversized_request() -> GraphExtractionRequest {
    let mut request = sample_request();
    request.chunk.content = "Alpha ".repeat(20_000);
    request
}

mod parsing;
mod prompt;
mod runtime;

#[test]
fn aggregate_provider_usage_json_keeps_a_unanimous_actual_service_tier() {
    let samples = vec![
        serde_json::json!({"prompt_tokens": 10, "completion_tokens": 2, "service_tier": "flex"}),
        serde_json::json!({"prompt_tokens": 20, "completion_tokens": 4, "service_tier": "flex"}),
    ];

    let aggregate = aggregate_provider_usage_json("openai", "gpt-6-luna", &samples);

    assert_eq!(aggregate["service_tier"], "flex");
    assert_eq!(aggregate["prompt_tokens"], 30);
    assert_eq!(aggregate["completion_tokens"], 6);
}

#[test]
fn aggregate_provider_usage_json_drops_a_mixed_or_absent_service_tier() {
    let mixed = vec![
        serde_json::json!({"prompt_tokens": 1, "service_tier": "flex"}),
        serde_json::json!({"prompt_tokens": 2, "service_tier": "default"}),
    ];
    assert!(
        aggregate_provider_usage_json("openai", "gpt-6-luna", &mixed).get("service_tier").is_none()
    );

    let absent = vec![serde_json::json!({"prompt_tokens": 1})];
    assert!(
        aggregate_provider_usage_json("openai", "gpt-6-luna", &absent)
            .get("service_tier")
            .is_none()
    );
}
