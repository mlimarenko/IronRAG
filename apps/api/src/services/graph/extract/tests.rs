use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use super::parse::{normalize_graph_extraction_output, parse_graph_extraction_output};
use super::prompt::{
    GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES, build_graph_extraction_prompt,
    build_graph_extraction_prompt_plan, build_graph_extraction_prompt_preview,
    graph_extraction_response_format,
};
use super::session::{
    build_provider_usage_json, build_raw_output_json, resolve_graph_extraction_with_gateway,
};
use super::types::*;
use crate::{
    domains::ai::AiBindingPurpose,
    domains::{
        provider_profiles::{
            EffectiveProviderProfile, ProviderModelSelection, SupportedProviderKind,
        },
        runtime_graph::RuntimeNodeType,
        runtime_ingestion::RuntimeProviderFailureClass,
    },
    infra::repositories::{ChunkRow, DocumentRow},
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::extraction_recovery::ExtractionRecoveryService,
    },
    shared::extraction::technical_facts::TechnicalFactQualifier,
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
        content: "OpenAI supplies embeddings for the annual report graph.".to_string(),
        token_count: None,
        metadata_json: serde_json::json!({}),
        created_at: chrono::Utc::now(),
    }
}

fn sample_profile() -> EffectiveProviderProfile {
    EffectiveProviderProfile {
        indexing: ProviderModelSelection {
            provider_kind: SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4-mini".to_string(),
        },
        embedding: ProviderModelSelection {
            provider_kind: SupportedProviderKind::OpenAi,
            model_name: "text-embedding-3-small".to_string(),
        },
        answer: ProviderModelSelection {
            provider_kind: SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4".to_string(),
        },
        vision: ProviderModelSelection {
            provider_kind: SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4-mini".to_string(),
        },
    }
}

fn sample_runtime_binding() -> ResolvedRuntimeBinding {
    ResolvedRuntimeBinding {
        binding_id: uuid::Uuid::now_v7(),
        workspace_id: uuid::Uuid::nil(),
        library_id: uuid::Uuid::nil(),
        binding_purpose: AiBindingPurpose::ExtractGraph,
        provider_catalog_id: uuid::Uuid::now_v7(),
        provider_kind: "openai".to_string(),
        provider_base_url: None,
        provider_api_style: "openai".to_string(),
        credential_id: uuid::Uuid::now_v7(),
        api_key: Some("test-api-key".to_string()),
        model_catalog_id: uuid::Uuid::now_v7(),
        model_name: "gpt-5.4-mini".to_string(),
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
                canonical_value: "GET".to_string(),
                display_value: "GET".to_string(),
                qualifiers: Vec::new(),
            },
            GraphExtractionTechnicalFact {
                fact_kind: "endpoint_path".to_string(),
                canonical_value: "/annual-report/graph".to_string(),
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
    }
}

fn oversized_request() -> GraphExtractionRequest {
    let mut request = sample_request();
    request.chunk.content = "Alpha ".repeat(20_000);
    request
}

#[test]
fn prompt_mentions_json_contract_and_chunk_text() {
    let prompt = build_graph_extraction_prompt(&sample_request());

    assert!(prompt.contains("strict JSON"));
    assert!(prompt.contains("entities"));
    assert!(prompt.contains("annual report graph"));
    assert!(prompt.contains("Chunk kind"));
    assert!(prompt.contains("technical_facts"));
    assert!(prompt.contains("copied verbatim from this catalog"));
    assert!(!prompt.contains("`topic`, or `document`"));
}

#[test]
fn downgraded_prompt_plan_reduces_segment_count_and_marks_shape() {
    let mut request = oversized_request();
    request.resume_hint = Some(GraphExtractionResumeHint { replay_count: 4, downgrade_level: 1 });

    let plan = build_graph_extraction_prompt_plan(
        &request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        256 * 1024,
    );

    assert!(plan.request_shape_key.contains("downgrade_1"));
    assert!(plan.request_size_bytes < 256 * 1024);
    assert!(plan.prompt.contains("Adaptive downgrade level: 1"));
}

#[test]
fn response_format_enum_matches_canonical_relation_catalog() {
    let response_format = graph_extraction_response_format("openai");
    let enum_values = response_format
        .get("json_schema")
        .and_then(|value| value.get("schema"))
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.get("relations"))
        .and_then(|value| value.get("items"))
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.get("relation_type"))
        .and_then(|value| value.get("enum"))
        .and_then(serde_json::Value::as_array)
        .expect("relation_type enum");
    let rendered =
        enum_values.iter().map(|value| value.as_str().expect("enum string")).collect::<Vec<_>>();

    assert_eq!(rendered, crate::services::graph::identity::canonical_relation_type_catalog());
}

#[test]
fn deepseek_uses_json_object_response_format() {
    let response_format = graph_extraction_response_format("deepseek");

    assert_eq!(
        response_format.get("type").and_then(serde_json::Value::as_str),
        Some("json_object")
    );
    assert!(response_format.get("json_schema").is_none());
}

#[test]
fn normalizes_json_and_string_candidates() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            "Annual report",
            { "label": "OpenAI", "node_type": "topic", "aliases": ["Open AI"], "summary": "provider" }
          ],
          "relations": [
            { "source": "Annual report", "target": "OpenAI", "type": "mentions" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.entities.len(), 2);
    assert_eq!(normalized.entities[0].label, "Annual report");
    assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Concept);
    assert_eq!(normalized.relations[0].relation_type, "mentions");
}

#[test]
fn accepts_expanded_node_type_values() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Valid", "node_type": "topic", "aliases": [], "summary": "" },
            { "label": "Google", "node_type": "organization", "aliases": [], "summary": "" }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 2);
    assert_eq!(normalized.entities[0].label, "Valid");
    assert_eq!(normalized.entities[0].node_type, RuntimeNodeType::Concept);
    assert_eq!(normalized.entities[1].label, "Google");
    assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Organization);
}

#[test]
fn falls_back_unknown_node_type_to_entity() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Something", "node_type": "invented_type", "aliases": [], "summary": "" }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "Something");
    assert_eq!(normalized.entities[0].node_type, RuntimeNodeType::Entity);
}

#[test]
fn rejects_json_inside_markdown_fence() {
    let error = parse_graph_extraction_output("```json\n{\"entities\":[],\"relations\":[]}\n```")
        .expect_err("fenced output must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn drops_empty_candidates_and_normalizes_relation_labels() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "  ", "node_type": "entity" },
            { "label": "DeepSeek", "aliases": ["", " Deep Seek "] }
          ],
          "relations": [
            { "source_label": "DeepSeek", "target_label": "Knowledge Graph", "relation_type": "Builds On" },
            { "source_label": " ", "target_label": "Ignored", "relation_type": "mentions" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "DeepSeek");
    assert_eq!(normalized.entities[0].aliases, vec!["Deep Seek".to_string()]);
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "builds_on");
}

#[test]
fn drops_semantically_void_relation_types_at_parse_time() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [],
          "relations": [
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "unknown" },
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "supports");
}

#[test]
fn drops_non_canonical_non_ascii_relation_types_at_parse_time() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [],
          "relations": [
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "включает" },
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "supports");
}

#[test]
fn rejects_non_json_payloads() {
    let error = parse_graph_extraction_output("not valid json").expect_err("invalid json");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_json_object_surrounded_by_prose() {
    let error = parse_graph_extraction_output(
        "Here is the result:\n{\"entities\":[\"OpenAI\"],\"relations\":[]}\nThanks.",
    )
    .expect_err("prose wrapper must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_json5_style_payloads() {
    let error = parse_graph_extraction_output(
        "{entities:[{label:'OpenAI', node_type:'entity', aliases:['Open AI'], summary:'provider',},], relations:[]}",
    )
    .expect_err("json5 payload must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_truncated_json_payloads() {
    let error = parse_graph_extraction_output(
        r#"{"entities":[{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}],"relations":[{"source_label":"OpenAI","target_label":"Graph","relation_type":"mentions","summary":"link"}"#,
    )
    .expect_err("truncated payload must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_named_sections_without_outer_object() {
    let error = normalize_graph_extraction_output(
        r#"
        entities:
        [{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}]
        relations:
        [{"source_label":"OpenAI","target_label":"Annual report","relation_type":"mentions","summary":"citation"}]
        "#,
    )
    .expect_err("named sections must fail");

    assert!(error.parse_error.contains("malformed_output"));
}

#[tokio::test]
async fn retries_after_terminal_parse_failure_and_aggregates_usage() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: "this is not json".to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 11,
                    "completion_tokens": 4,
                    "total_tokens": 15,
                }),
            }),
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10,
                }),
            }),
        ]),
    };

    let resolved = resolve_graph_extraction_with_gateway(
        &gateway,
        &ExtractionRecoveryService,
        &crate::services::ops::provider_failure::ProviderFailureClassificationService::default(),
        &sample_profile(),
        &sample_runtime_binding(),
        &sample_request(),
        true,
        2,
        1,
    )
    .await
    .expect("retry should recover");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(resolved.recovery.reask_count, 1);
    assert_eq!(resolved.usage_json.get("call_count").and_then(serde_json::Value::as_u64), Some(2));
    assert_eq!(
        resolved.usage_json.get("total_tokens").and_then(serde_json::Value::as_i64),
        Some(25)
    );
    let raw_output_json = build_raw_output_json(
        &resolved.output_text,
        resolved.usage_json.clone(),
        &resolved.lifecycle,
        &resolved.recovery,
        &resolved.recovery_summary,
        &resolved.usage_calls,
    );
    let provider_calls = raw_output_json
        .get("provider_calls")
        .and_then(serde_json::Value::as_array)
        .expect("provider calls are persisted");
    assert_eq!(provider_calls.len(), 2);
    assert!(
        provider_calls[0]
            .get("timing")
            .and_then(|value| value.get("elapsed_ms"))
            .and_then(serde_json::Value::as_i64)
            .is_some()
    );
}

#[tokio::test]
async fn retries_upstream_protocol_failures_as_transient_provider_errors() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Err(anyhow::anyhow!(
                "{}",
                "provider request failed: provider=openai status=400 body={\"error\":{\"message\":\"We could not parse the JSON body of your request. The OpenAI API expects a JSON payload.\",\"type\":\"invalid_request_error\"}}"
            )),
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 9,
                    "completion_tokens": 3,
                    "total_tokens": 12,
                }),
            }),
        ]),
    };

    let resolved = resolve_graph_extraction_with_gateway(
        &gateway,
        &ExtractionRecoveryService,
        &crate::services::ops::provider_failure::ProviderFailureClassificationService::default(),
        &sample_profile(),
        &sample_runtime_binding(),
        &sample_request(),
        true,
        2,
        1,
    )
    .await
    .expect("upstream protocol failure should retry");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(
        resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
    );
    assert_eq!(
        resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
        Some("upstream_protocol_failure")
    );
}

#[tokio::test]
async fn retries_transient_upstream_rejections_as_provider_errors() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Err(anyhow::anyhow!(
                "{}",
                "provider request failed: provider=openai status=520 body={\"raw_body\":\"error code: 520\"}"
            )),
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 11,
                    "completion_tokens": 4,
                    "total_tokens": 15,
                }),
            }),
        ]),
    };

    let resolved = resolve_graph_extraction_with_gateway(
        &gateway,
        &ExtractionRecoveryService,
        &crate::services::ops::provider_failure::ProviderFailureClassificationService::default(),
        &sample_profile(),
        &sample_runtime_binding(),
        &sample_request(),
        true,
        2,
        1,
    )
    .await
    .expect("transient upstream rejection should retry");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(
        resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
    );
    assert_eq!(
        resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
        Some("upstream_transient_rejection")
    );
}

#[test]
fn prompt_preview_is_deterministic_for_large_chunks() {
    let request = oversized_request();
    let (first_prompt, first_shape, first_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);
    let (second_prompt, second_shape, second_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);

    assert_eq!(first_prompt, second_prompt);
    assert_eq!(first_shape, second_shape);
    assert_eq!(first_size, second_size);
    assert!(first_prompt.contains("[chunk_segment_1]"));
    assert!(first_shape.contains("segments_3"));
    assert!(first_size <= 8 * 1024 + GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES);
}

#[tokio::test]
async fn fails_after_retry_exhaustion_with_recovery_trace() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: "broken payload".to_string(),
                usage_json: serde_json::json!({ "prompt_tokens": 5 }),
            }),
            Ok(ChatResponse {
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                output_text: "still broken".to_string(),
                usage_json: serde_json::json!({ "prompt_tokens": 6 }),
            }),
        ]),
    };

    let failure = resolve_graph_extraction_with_gateway(
        &gateway,
        &ExtractionRecoveryService,
        &crate::services::ops::provider_failure::ProviderFailureClassificationService::default(),
        &sample_profile(),
        &sample_runtime_binding(),
        &sample_request(),
        true,
        2,
        1,
    )
    .await
    .expect_err("malformed output should fail after retry exhaustion");

    assert!(failure.error_message.contains("after 2 provider attempt(s)"));
    assert_eq!(
        failure.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::InvalidModelOutput)
    );
}

#[test]
fn provider_usage_payload_keeps_provider_metadata() {
    let usage = build_provider_usage_json(
        "openai",
        "gpt-5.4-mini",
        serde_json::json!({
            "prompt_tokens": 21,
            "completion_tokens": 9,
        }),
    );

    assert_eq!(usage.get("provider_kind").and_then(serde_json::Value::as_str), Some("openai"));
    assert_eq!(usage.get("model_name").and_then(serde_json::Value::as_str), Some("gpt-5.4-mini"));
    assert_eq!(usage.get("prompt_tokens").and_then(serde_json::Value::as_i64), Some(21));
}
