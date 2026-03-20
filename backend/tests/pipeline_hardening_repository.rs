mod pipeline_hardening_support;

use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use pipeline_hardening_support::{
    sample_queue_slice_row, sample_settlement_rollup_input, sample_settlement_row, sample_warning,
    sample_warning_row,
};
use rustrag_backend::{
    domains::runtime_ingestion::{
        RuntimeCollectionWarning, RuntimeOperatorWarningKind, RuntimeOperatorWarningScope,
        RuntimeQueueWaitingReason,
    },
    infra::repositories::{
        RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL,
        RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL,
        RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL,
        RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL,
        REPLACE_RUNTIME_COLLECTION_SETTLEMENT_ROLLUP_INSERT_SQL,
        REPLACE_RUNTIME_COLLECTION_WARNING_SNAPSHOT_INSERT_SQL,
        UPSERT_RUNTIME_COLLECTION_SETTLEMENT_SNAPSHOT_SQL, build_runtime_collection_warning_rows,
        build_runtime_library_queue_slice_snapshot, classify_runtime_collection_warning_rows,
        normalize_runtime_collection_settlement_rollup_inputs, parse_runtime_queue_waiting_reason,
        runtime_queue_waiting_reason_key,
    },
};

#[test]
fn queue_slice_snapshot_builder_preserves_runtime_truth() {
    let row = sample_queue_slice_row();
    let snapshot = build_runtime_library_queue_slice_snapshot(&row, 1, 0);

    assert_eq!(snapshot.project_id, row.project_id);
    assert_eq!(snapshot.workspace_id, row.workspace_id);
    assert_eq!(snapshot.queued_count, row.queued_count);
    assert_eq!(snapshot.processing_count, row.processing_count);
    assert_eq!(snapshot.isolated_capacity_count, 1);
    assert_eq!(snapshot.available_capacity_count, 0);
    assert_eq!(snapshot.waiting_reason.as_deref(), Some("isolated_capacity_wait"));
}

#[test]
fn queue_waiting_reason_round_trips_through_repository_keys() {
    let waiting_reason = parse_runtime_queue_waiting_reason(Some("isolated_capacity_wait"));

    assert_eq!(waiting_reason, Some(RuntimeQueueWaitingReason::IsolatedCapacityWait));
    assert_eq!(runtime_queue_waiting_reason_key(&RuntimeQueueWaitingReason::Blocked), "blocked");
}

#[test]
fn warning_row_builder_deduplicates_and_classifies_degraded_truth() {
    let project_id = Uuid::now_v7();
    let computed_at = Utc::now();
    let informational = sample_warning();
    let degraded = RuntimeCollectionWarning {
        warning_kind: RuntimeOperatorWarningKind::MissingAccounting,
        warning_scope: RuntimeOperatorWarningScope::Collection,
        warning_message: "Some provider work is still missing settled accounting.".to_string(),
        is_degraded: true,
    };
    let rows = build_runtime_collection_warning_rows(
        project_id,
        &[informational.clone(), informational, degraded.clone()],
        computed_at,
    );
    let (informational_rows, degraded_rows) = classify_runtime_collection_warning_rows(&rows);

    assert_eq!(rows.len(), 2);
    assert_eq!(informational_rows.len(), 1);
    assert_eq!(degraded_rows.len(), 1);
    assert_eq!(degraded_rows[0].warning_kind, "missing_accounting");
    assert!(degraded_rows[0].is_degraded);
    assert_eq!(rows[0].project_id, project_id);
    assert_eq!(rows[0].computed_at, computed_at);
}

#[test]
fn warning_classification_keeps_sample_row_informational() {
    let row = sample_warning_row();
    let (informational_rows, degraded_rows) =
        classify_runtime_collection_warning_rows(std::slice::from_ref(&row));

    assert_eq!(informational_rows.len(), 1);
    assert!(degraded_rows.is_empty());
    assert_eq!(informational_rows[0].warning_kind, "ordinary_backlog");
}

#[test]
fn settlement_rollup_normalization_keeps_single_primary_bottleneck() {
    let settlement = sample_settlement_row();
    let primary = sample_settlement_rollup_input();
    let duplicate_primary = sample_settlement_rollup_input();
    let secondary = sample_settlement_rollup_input();
    let normalized = normalize_runtime_collection_settlement_rollup_inputs(
        "stage",
        &[
            primary,
            duplicate_primary,
            rustrag_backend::infra::repositories::RuntimeCollectionSettlementRollupInput {
                scope_kind: String::new(),
                scope_key: "embedding_chunks".to_string(),
                queued_count: 1,
                processing_count: 1,
                completed_count: 24,
                failed_count: 0,
                document_count: 24,
                ready_count: 0,
                ready_no_graph_count: 0,
                content_extracted_count: 0,
                chunked_count: 0,
                embedded_count: 0,
                graph_active_count: 0,
                graph_ready_count: 0,
                live_estimated_cost: Some(Decimal::new(12, 2)),
                settled_estimated_cost: Some(Decimal::new(150, 2)),
                missing_estimated_cost: Some(Decimal::ZERO),
                currency: Some("USD".to_string()),
                avg_elapsed_ms: Some(4_200),
                max_elapsed_ms: Some(12_000),
                bottleneck_stage: None,
                bottleneck_avg_elapsed_ms: None,
                bottleneck_max_elapsed_ms: None,
                prompt_tokens: 4_200,
                completion_tokens: 200,
                total_tokens: 4_400,
                accounting_status: "priced".to_string(),
                bottleneck_rank: Some(2),
                is_primary_bottleneck: true,
            },
            secondary,
        ],
    );

    assert_eq!(normalized.len(), 4);
    assert_eq!(normalized.iter().filter(|row| row.is_primary_bottleneck).count(), 1);
    assert!(normalized.iter().all(|row| row.scope_kind == "stage"));
    assert_eq!(normalized[0].scope_key, "extracting_graph");
    assert_eq!(settlement.progress_state, "settling");
}

#[test]
fn settlement_snapshot_upsert_sql_covers_all_bound_columns() {
    for placeholder in 1..=32 {
        assert!(
            UPSERT_RUNTIME_COLLECTION_SETTLEMENT_SNAPSHOT_SQL.contains(&format!("${placeholder}")),
            "missing placeholder ${placeholder} in settlement snapshot upsert SQL",
        );
    }
    assert!(
        !UPSERT_RUNTIME_COLLECTION_SETTLEMENT_SNAPSHOT_SQL.contains("$33"),
        "unexpected extra placeholder in settlement snapshot upsert SQL",
    );
    assert!(
        UPSERT_RUNTIME_COLLECTION_SETTLEMENT_SNAPSHOT_SQL.contains("is distinct from"),
        "settlement snapshot upsert SQL must skip noop updates",
    );
}

#[test]
fn settlement_rollup_insert_sql_covers_all_bound_columns() {
    for placeholder in 1..=30 {
        assert!(
            REPLACE_RUNTIME_COLLECTION_SETTLEMENT_ROLLUP_INSERT_SQL
                .contains(&format!("${placeholder}")),
            "missing placeholder ${placeholder} in settlement rollup insert SQL",
        );
    }
    assert!(
        !REPLACE_RUNTIME_COLLECTION_SETTLEMENT_ROLLUP_INSERT_SQL.contains("$31"),
        "unexpected extra placeholder in settlement rollup insert SQL",
    );
    assert!(
        REPLACE_RUNTIME_COLLECTION_SETTLEMENT_ROLLUP_INSERT_SQL
            .contains("on conflict (project_id, scope_kind, scope_key) do update"),
        "settlement rollup insert SQL must be idempotent under concurrent refresh",
    );
    assert!(
        REPLACE_RUNTIME_COLLECTION_SETTLEMENT_ROLLUP_INSERT_SQL.contains("is distinct from"),
        "settlement rollup insert SQL must skip noop updates",
    );
}

#[test]
fn warning_snapshot_insert_sql_is_idempotent_under_concurrent_refresh() {
    assert!(
        REPLACE_RUNTIME_COLLECTION_WARNING_SNAPSHOT_INSERT_SQL
            .contains("on conflict (project_id, warning_kind, warning_scope) do update"),
        "warning snapshot insert SQL must be idempotent under concurrent refresh",
    );
    assert!(
        REPLACE_RUNTIME_COLLECTION_WARNING_SNAPSHOT_INSERT_SQL.contains("is distinct from"),
        "warning snapshot insert SQL must skip noop updates",
    );
}

#[test]
fn graph_support_recount_sql_uses_grouped_evidence_counts_instead_of_correlated_subqueries() {
    for sql in [
        RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL,
        RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL,
        RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL,
        RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL,
    ] {
        assert!(
            sql.contains("group by evidence.target_id"),
            "support recount SQL must aggregate evidence by target id",
        );
        assert!(
            sql.contains("left join evidence_counts"),
            "support recount SQL must join grouped evidence counts once",
        );
        assert!(
            sql.contains("is distinct from desired_counts.support_count"),
            "support recount SQL must skip noop support_count updates",
        );
        assert!(
            !sql.contains("select count(*)::integer\n             from runtime_graph_evidence as evidence\n             where evidence.project_id = $1\n               and evidence.target_kind"),
            "support recount SQL must not use per-row correlated evidence recounts",
        );
    }
}
