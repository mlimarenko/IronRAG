use anyhow::Context;
use uuid::Uuid;

use crate::app::state::AppState;
use crate::infra::repositories::content_document_list_repository::{
    DocumentListPageQuery, DocumentListSortColumn, list_document_page_rows,
};

use super::super::{
    retrieve::{load_latest_library_generation, query_graph_status},
    types::{
        RuntimeQueryLibraryContext, RuntimeQueryLibrarySummary, RuntimeQueryRecentDocument,
        RuntimeQueryWarning,
    },
};

pub(crate) async fn load_query_execution_library_context(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<RuntimeQueryLibraryContext> {
    let generation = load_latest_library_generation(state, library_id).await?;
    let graph_status = query_graph_status(generation.as_ref());

    // Primary O(1) path — no more `list_documents` N+1 storm. Three
    // bounded queries: one Postgres aggregate for the summary counts,
    // one `runtime_graph_snapshot` point lookup, and one keyset page
    // (limit 12) for the recent-documents section fed into the prompt.
    // The previous implementation enumerated every document plus multiple
    // per-document prefetches per call, which on a 5k-doc library burned ~180 s per
    // query turn before the outer timeout cut it off.
    let metrics =
        crate::infra::repositories::content_document_list_repository::aggregate_library_document_metrics(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to aggregate library metrics for query context")?;
    let empty_filters =
        crate::infra::repositories::content_document_list_repository::DocumentListFilters::default(
        );
    let empty_ranges =
        crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges::default();
    let recent_page = list_document_page_rows(
        &state.persistence.postgres,
        DocumentListPageQuery {
            library_id,
            include_deleted: false,
            cursor: None,
            limit: 12,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &empty_filters,
            search_ranges: &empty_ranges,
            snapshot_at: chrono::Utc::now(),
            sort: DocumentListSortColumn::CreatedAt,
            sort_desc: true,
            status_filter: &[],
            id_filter: None,
        },
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.to_string()))
    .context("failed to load recent document rows for query context")?;

    let in_flight = metrics.processing + metrics.queued;
    // Backlog surfaced to the convergence-warning classifier covers
    // everything that is not yet readable — jobs still in flight
    // plus any queued / canceled retries the runtime will sweep
    // before the library reaches a fully-ready state. Derived from
    // the s row so this number and the dashboard
    // `in-flight` card always agree.
    let backlog_count = in_flight;
    let convergence_status = query_execution_convergence_status(graph_status, in_flight);
    let summary = RuntimeQueryLibrarySummary {
        document_count: usize::try_from(metrics.total).unwrap_or(0),
        // Authoritative `graph_ready` comes from the metrics row (already
        // clamped to `ready` so the published invariant holds).
        graph_ready_count: usize::try_from(metrics.graph_ready).unwrap_or(0),
        processing_count: usize::try_from(in_flight).unwrap_or(0),
        failed_count: usize::try_from(metrics.failed + metrics.canceled).unwrap_or(0),
        graph_status,
    };
    let recent_documents =
        summarize_recent_query_documents_from_rows(&recent_page.rows, graph_status);
    Ok(RuntimeQueryLibraryContext {
        summary,
        recent_documents,
        warning: query_execution_convergence_warning(state, convergence_status, backlog_count),
    })
}

fn summarize_recent_query_documents_from_rows(
    rows: &[crate::infra::repositories::content_document_list_repository::ContentDocumentListRow],
    graph_status: &'static str,
) -> Vec<RuntimeQueryRecentDocument> {
    rows.iter()
        .map(|row| {
            let title = row
                .revision_title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| row.external_key.clone());
            let pipeline_state =
                match (row.job_queue_state.as_deref(), row.mutation_state.as_deref()) {
                    (Some("failed"), _) | (_, Some("failed")) => "failed",
                    (Some("leased"), _) => "processing",
                    _ if row.readable_revision_id.is_some() => "ready",
                    (Some("canceled"), _) | (_, Some("canceled")) => "failed",
                    (Some("queued"), _) | (_, Some("accepted" | "running")) => "queued",
                    _ => "pending",
                };
            let graph_state = if pipeline_state == "ready" && graph_status == "current" {
                "ready"
            } else if pipeline_state == "failed" {
                "failed"
            } else {
                "pending"
            };
            RuntimeQueryRecentDocument {
                title,
                uploaded_at: row.created_at.to_rfc3339(),
                mime_type: row.revision_mime_type.clone(),
                pipeline_state,
                graph_state,
                preview_excerpt: None,
            }
        })
        .collect()
}

fn query_execution_convergence_status(graph_status: &str, backlog_count: i64) -> &'static str {
    if backlog_count > 0 || !matches!(graph_status, "current") {
        return "partial";
    }
    "current"
}

fn query_execution_convergence_warning(
    state: &AppState,
    convergence_status: &str,
    backlog_count: i64,
) -> Option<RuntimeQueryWarning> {
    if convergence_status != "partial" {
        return None;
    }

    let threshold =
        i64::try_from(state.bulk_ingest_hardening.graph_convergence_warning_backlog_threshold)
            .unwrap_or(1);
    if backlog_count < threshold {
        return None;
    }

    Some(RuntimeQueryWarning {
        warning: format!(
            "Graph coverage is still converging while {backlog_count} document or mutation task(s) remain in backlog."
        ),
        warning_kind: "partial_convergence",
    })
}
