use chrono::Utc;
use rustrag_backend::{
    domains::runtime_ingestion::RuntimeProviderFailureClass,
    infra::repositories::RuntimeGraphExtractionResumeRollupRow,
    services::{
        provider_failure_classification::ProviderFailureClassificationService,
        runtime_ingestion::build_runtime_document_graph_throughput_summary,
    },
};
use uuid::Uuid;

#[test]
fn provider_failure_classes_remain_distinct() {
    let service = ProviderFailureClassificationService::default();

    let timeout = service.classify_failure(
        "openai",
        "gpt-5-mini",
        "provider request failed: provider=openai status=504 body={}",
        "graph_extract_v3:initial:segments_3:trimmed",
        32_000,
        Some(1),
        Some(30_000),
        Some("retrying_provider_call".to_string()),
        false,
    );
    let rejection = service.classify_failure(
        "openai",
        "gpt-5-mini",
        "provider request failed: provider=openai status=429 body={}",
        "graph_extract_v3:initial:segments_1:full",
        4_000,
        Some(1),
        Some(1_000),
        Some("terminal_failure".to_string()),
        false,
    );
    let invalid_output = service.classify_failure(
        "openai",
        "gpt-5-mini",
        "invalid model output: schema mismatch",
        "graph_extract_v3:provider_retry:segments_1:full",
        4_000,
        Some(1),
        Some(800),
        Some("terminal_failure".to_string()),
        true,
    );
    let recovered = service.summarize(
        RuntimeProviderFailureClass::RecoveredAfterRetry,
        Some("openai".to_string()),
        Some("gpt-5-mini".to_string()),
        Some("graph_extract_v3:provider_retry:segments_1:full".to_string()),
        Some(4_000),
        Some(1),
        None,
        Some(900),
        Some("recovered_after_retry".to_string()),
        true,
    );

    assert_eq!(timeout.failure_class, RuntimeProviderFailureClass::UpstreamTimeout);
    assert_eq!(timeout.upstream_status.as_deref(), Some("504"));
    assert_eq!(rejection.failure_class, RuntimeProviderFailureClass::UpstreamRejection);
    assert_eq!(rejection.upstream_status.as_deref(), Some("429"));
    assert_eq!(invalid_output.failure_class, RuntimeProviderFailureClass::InvalidModelOutput);
    assert_eq!(recovered.failure_class, RuntimeProviderFailureClass::RecoveredAfterRetry);
}

#[test]
fn resume_rollup_is_visible_in_document_graph_throughput_summary() {
    let checkpoint = rustrag_backend::infra::repositories::RuntimeGraphProgressCheckpointRow {
        ingestion_run_id: Uuid::now_v7(),
        attempt_no: 3,
        processed_chunks: 12,
        total_chunks: 20,
        progress_percent: Some(60),
        provider_call_count: 8,
        avg_call_elapsed_ms: Some(900),
        avg_chunk_elapsed_ms: Some(1_200),
        avg_chars_per_second: Some(250.0),
        avg_tokens_per_second: Some(35.0),
        last_provider_call_at: Some(Utc::now()),
        next_checkpoint_eta_ms: Some(2_000),
        pressure_kind: Some("provider_bound".to_string()),
        provider_failure_class: None,
        request_shape_key: None,
        request_size_bytes: None,
        upstream_status: None,
        retry_outcome: None,
        computed_at: Utc::now(),
    };
    let resume_rollup = RuntimeGraphExtractionResumeRollupRow {
        ingestion_run_id: checkpoint.ingestion_run_id,
        chunk_count: 20,
        ready_chunk_count: 12,
        failed_chunk_count: 0,
        replayed_chunk_count: 5,
        resume_hit_count: 7,
        resumed_chunk_count: 4,
        max_downgrade_level: 2,
    };

    let summary = build_runtime_document_graph_throughput_summary(
        Some(&checkpoint),
        Some(&resume_rollup),
        Some(1),
    )
    .expect("graph throughput summary should be built");

    assert_eq!(summary.resumed_chunk_count, 4);
    assert_eq!(summary.resume_hit_count, 7);
    assert_eq!(summary.replayed_chunk_count, 5);
    assert_eq!(summary.max_downgrade_level, 2);
    assert_eq!(summary.bottleneck_rank, Some(1));
    assert!(summary.duplicate_work_ratio.unwrap_or_default() > 0.2);
}
