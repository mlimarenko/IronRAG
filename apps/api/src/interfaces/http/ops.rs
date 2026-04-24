use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ingest,
        knowledge::{KnowledgeLibraryGeneration, KnowledgeLibrarySummary},
        ops::{OpsAsyncOperation, OpsAsyncOperationProgress, OpsLibraryState, OpsLibraryWarning},
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_USAGE_READ, load_async_operation_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
};
use ironrag_contracts::{
    diagnostics::{MessageLevel, OperatorWarning},
    documents::{
        DashboardAttentionItem, DashboardMetric, DashboardSurface, DocumentReadiness,
        DocumentStatus, DocumentSummary, DocumentsOverview, WebIngestRunState, WebIngestRunSummary,
        WebRunCounts,
    },
    graph::{
        GraphConvergenceStatus, GraphGenerationSummary, GraphReadinessSummary, GraphStatus,
        GraphSurface,
    },
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpsLibraryStateSummaryResponse {
    pub library_id: Uuid,
    pub queue_depth: i64,
    pub running_attempts: i64,
    pub readable_document_count: i64,
    pub failed_document_count: i64,
    pub degraded_state: String,
    pub latest_knowledge_generation_id: Option<Uuid>,
    pub knowledge_generation_state: Option<String>,
    pub last_recomputed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpsLibraryWarningResponse {
    pub id: Uuid,
    pub library_id: Uuid,
    pub warning_kind: String,
    pub severity: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeGenerationResponse {
    pub id: Uuid,
    pub library_id: Uuid,
    pub generation_kind: String,
    pub generation_state: String,
    pub source_revision_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpsLibraryStateResponse {
    pub state: OpsLibraryStateSummaryResponse,
    pub knowledge_generations: Vec<KnowledgeGenerationResponse>,
    pub warnings: Vec<OpsLibraryWarningResponse>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ops/operations/{operation_id}", get(get_async_operation))
        .route("/ops/libraries/{library_id}", get(get_library_state))
        .route("/ops/libraries/{library_id}/dashboard", get(get_library_dashboard))
}

/// Canonical async-operation polling payload. Exposes the raw parent row
/// plus aggregated child-operation counts, so any batch endpoint (batch
/// rerun, batch delete, future batch annotate, …) can be polled via the
/// same response shape. `progress` is populated whenever at least one child
/// operation references this row via `parent_async_operation_id`; for
/// non-batch operations it reports zeros across the board.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsyncOperationDetailResponse {
    #[serde(flatten)]
    operation: OpsAsyncOperation,
    progress: OpsAsyncOperationProgress,
}

#[tracing::instrument(
    level = "info",
    name = "http.get_async_operation",
    skip_all,
    fields(operation_id = %operation_id)
)]
async fn get_async_operation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<AsyncOperationDetailResponse>, ApiError> {
    let _ =
        load_async_operation_and_authorize(&auth, &state, operation_id, POLICY_USAGE_READ).await?;
    let mut operation =
        state.canonical_services.ops.get_async_operation(&state, operation_id).await?;
    let progress =
        state.canonical_services.ops.get_async_operation_progress(&state, operation_id).await?;

    // For parent batch ops (children present), the effective status is
    // usually derived from child progress. Batch delete is the exception:
    // after every child delete settles, the parent still runs one library
    // graph projection refresh. Until that parent-owned finalization writes
    // completed_at, reporting `ready` would be a false terminal state.
    if progress.total > 0
        && !matches!(operation.status.as_str(), "failed" | "canceled" | "superseded")
    {
        let pending = progress.total.saturating_sub(progress.completed + progress.failed);
        let parent_finalizing = operation.operation_kind == "batch_delete_documents"
            && operation.completed_at.is_none();
        let derived = if pending > 0 || parent_finalizing {
            "processing"
        } else if progress.failed > 0 {
            "failed"
        } else {
            "ready"
        };
        if operation.status != derived {
            operation.status = derived.to_string();
        }
        if pending == 0 && !parent_finalizing && operation.completed_at.is_none() {
            operation.completed_at = Some(chrono::Utc::now());
        }
    }

    Ok(Json(AsyncOperationDetailResponse { operation, progress }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_library_state",
    skip_all,
    fields(library_id = %library_id)
)]
async fn get_library_state(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<OpsLibraryStateResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let snapshot =
        state.canonical_services.ops.get_library_state_snapshot(&state, library_id).await?;
    let warnings = state.canonical_services.ops.list_library_warnings(&state, library_id).await?;
    Ok(Json(OpsLibraryStateResponse {
        state: map_ops_library_state(&snapshot.state),
        knowledge_generations: snapshot
            .knowledge_generations
            .iter()
            .map(map_knowledge_generation)
            .collect(),
        warnings: warnings.iter().map(map_ops_warning).collect(),
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_library_dashboard",
    skip_all,
    fields(library_id = %library_id, elapsed_ms)
)]
async fn get_library_dashboard(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<DashboardSurface>, ApiError> {
    let started_at = std::time::Instant::now();
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;

    // Canonical bounded fetch — no more `list_documents` enumeration.
    // Top 6 recent entries for the "Recent documents" strip + the
    // aggregate status counts for the overview tiles. The old path
    // spent ~7.5 s on a 5k-doc library because it enumerated every
    // document through the 6-call prefetch pipeline for stats that
    // are a single `COUNT(*) FILTER (...)` away.
    let recent_page_command = crate::services::content::service::ListDocumentsPageCommand {
        library_id,
        include_deleted: false,
        cursor: None,
        limit: 6,
        search: None,
        sort: crate::infra::repositories::content_repository::DocumentListSortColumn::CreatedAt,
        sort_desc: true,
        status_filter: Vec::new(),
    };
    let (
        recent_page,
        document_metrics,
        recent_web_runs,
        knowledge_summary,
        ops_snapshot,
        ops_warnings,
    ) = tokio::try_join!(
        state.canonical_services.content.list_documents_page(&state, recent_page_command),
        async {
            crate::infra::repositories::content_repository::aggregate_library_document_metrics(
                &state.persistence.postgres,
                library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))
        },
        state.canonical_services.web_ingest.list_runs(&state, library_id, 6),
        state.canonical_services.knowledge.get_library_summary(&state, library_id),
        state.canonical_services.ops.get_library_state_snapshot(&state, library_id),
        state.canonical_services.ops.list_library_warnings(&state, library_id),
    )?;

    let recent_documents: Vec<DocumentSummary> =
        recent_page.items.into_iter().map(map_list_entry_to_dashboard_summary).collect();
    // `overview` is derived from the canonical `document_metrics` row
    // to keep the two fields on `DashboardSurface` consistent by
    // construction. Legacy UI consumers that read `overview.*` see
    // the same numbers as new consumers that read `documentMetrics`.
    let overview = build_documents_overview_from_metrics(&document_metrics);
    let warnings = map_operator_warnings(&ops_warnings, &ops_snapshot.state);
    let graph = map_graph_surface(&knowledge_summary, &ops_snapshot.state, warnings.first());
    let attention = build_attention_items_bounded(
        &ops_snapshot.state,
        &ops_warnings,
        &graph,
        &recent_documents,
    );
    let metrics = build_dashboard_metrics(&overview, &ops_snapshot.state, &graph, attention.len());
    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);

    Ok(Json(DashboardSurface {
        overview,
        document_metrics,
        metrics,
        recent_documents,
        recent_web_runs: recent_web_runs.into_iter().map(map_web_run_summary).collect(),
        graph,
        attention,
        warnings,
    }))
}

/// Builds a `DocumentSummary` for the dashboard "Recent documents" strip
/// from a slim `ContentDocumentListEntry`. All fields that require a
/// per-document Arango revision fetch are omitted — the dashboard
/// surface does not display them on this card.
fn map_list_entry_to_dashboard_summary(
    entry: crate::services::content::service::ContentDocumentListEntry,
) -> DocumentSummary {
    let status = parse_list_entry_status(&entry.status);
    let readiness = parse_list_entry_readiness(&entry.readiness);
    DocumentSummary {
        id: entry.id,
        workspace_id: Some(entry.workspace_id),
        library_id: Some(entry.library_id),
        file_name: entry.file_name,
        file_type: entry.file_type.unwrap_or_else(|| "unknown".to_string()),
        file_size: entry.file_size.unwrap_or(0),
        uploaded_at: entry.uploaded_at,
        status,
        readiness,
        stage_label: entry.stage,
        progress_percent: None,
        cost_usd: None,
        failure_message: entry.failure_code,
        can_retry: entry.retryable,
        prepared_segment_count: None,
        technical_fact_count: None,
        source_format: None,
    }
}

fn parse_list_entry_status(value: &str) -> DocumentStatus {
    // The dashboard contract enum now models `canceled` separately
    // (prior versions folded it into `Failed`, which hid the
    // difference between a pipeline rejection and an operator
    // withdrawal and made batch-cancel rollouts indistinguishable
    // from real failures on the dashboard). Unknown values still
    // degrade to `Queued` so the dashboard can't crash on a future
    // backend enum addition.
    match value {
        "ready" => DocumentStatus::Ready,
        "processing" => DocumentStatus::Processing,
        "queued" => DocumentStatus::Queued,
        "failed" => DocumentStatus::Failed,
        "canceled" => DocumentStatus::Canceled,
        _ => DocumentStatus::Queued,
    }
}

fn parse_list_entry_readiness(value: &str) -> DocumentReadiness {
    match value {
        "graph_ready" => DocumentReadiness::GraphReady,
        "graph_sparse" => DocumentReadiness::GraphSparse,
        "readable" => DocumentReadiness::Readable,
        "failed" => DocumentReadiness::Failed,
        _ => DocumentReadiness::Processing,
    }
}

/// Canonical path: derive the legacy `DocumentsOverview` shape from
/// a freshly-computed `LibraryDocumentMetrics`. Used by the dashboard
/// handler so both fields on `DashboardSurface` are built from the
/// same numbers. The previous `_from_counts` sibling was removed —
/// everything now consolidates on `LibraryDocumentMetrics`.
fn build_documents_overview_from_metrics(
    metrics: &ironrag_contracts::documents::LibraryDocumentMetrics,
) -> DocumentsOverview {
    DocumentsOverview {
        total_documents: saturating_i32(metrics.total.max(0) as usize),
        ready_documents: saturating_i32(metrics.ready.max(0) as usize),
        processing_documents: saturating_i32((metrics.processing + metrics.queued).max(0) as usize),
        failed_documents: saturating_i32((metrics.failed + metrics.canceled).max(0) as usize),
        graph_sparse_documents: saturating_i32(metrics.graph_sparse.max(0) as usize),
    }
}

fn build_attention_items_bounded(
    ops_state: &OpsLibraryState,
    warnings: &[OpsLibraryWarning],
    graph: &GraphSurface,
    recent_documents: &[DocumentSummary],
) -> Vec<DashboardAttentionItem> {
    let mut attention = Vec::new();
    let graph_coverage_gap_count = usize::try_from(graph.graph_sparse_document_count).unwrap_or(0);

    if ops_state.failed_document_count > 0 {
        attention.push(DashboardAttentionItem {
            code: "failed_documents".to_string(),
            title: "Failed documents need review".to_string(),
            detail: format!(
                "{} documents are currently failed in the active library.",
                ops_state.failed_document_count
            ),
            route_path: "/documents".to_string(),
            level: MessageLevel::Error,
        });
    }

    if graph_coverage_gap_count > 0 {
        attention.push(DashboardAttentionItem {
            code: "graph_coverage_gap".to_string(),
            title: "Graph coverage remains partial".to_string(),
            detail: format!(
                "{graph_coverage_gap_count} readable documents still do not contribute to the graph."
            ),
            route_path: "/documents?status=processing".to_string(),
            level: MessageLevel::Warning,
        });
    }

    if let Some(document) = recent_documents.iter().find(|document| document.can_retry) {
        attention.push(DashboardAttentionItem {
            code: "retryable_document".to_string(),
            title: "A document can be retried".to_string(),
            detail: format!(
                "{} reported a retryable failure or stalled ingest step.",
                document.file_name
            ),
            route_path: "/documents".to_string(),
            level: MessageLevel::Warning,
        });
    }

    attention.extend(warnings.iter().map(map_attention_item));
    attention.sort_by(|left, right| {
        attention_priority(right.level)
            .cmp(&attention_priority(left.level))
            .then_with(|| left.code.cmp(&right.code))
    });
    attention.dedup_by(|left, right| left.code == right.code);
    attention.truncate(6);
    attention
}

fn map_ops_library_state(state: &OpsLibraryState) -> OpsLibraryStateSummaryResponse {
    OpsLibraryStateSummaryResponse {
        library_id: state.library_id,
        queue_depth: state.queue_depth,
        running_attempts: state.running_attempts,
        readable_document_count: state.readable_document_count,
        failed_document_count: state.failed_document_count,
        degraded_state: state.degraded_state.clone(),
        latest_knowledge_generation_id: state.latest_knowledge_generation_id,
        knowledge_generation_state: state.knowledge_generation_state.clone(),
        last_recomputed_at: state.last_recomputed_at,
    }
}

fn map_knowledge_generation(
    generation: &KnowledgeLibraryGeneration,
) -> KnowledgeGenerationResponse {
    KnowledgeGenerationResponse {
        id: generation.id,
        library_id: generation.library_id,
        generation_kind: generation.generation_kind.clone(),
        generation_state: generation.generation_state.clone(),
        source_revision_id: generation.source_revision_id,
        created_at: generation.created_at,
        completed_at: generation.completed_at,
    }
}

fn map_ops_warning(warning: &OpsLibraryWarning) -> OpsLibraryWarningResponse {
    OpsLibraryWarningResponse {
        id: warning.id,
        library_id: warning.library_id,
        warning_kind: warning.warning_kind.clone(),
        severity: warning.severity.clone(),
        created_at: warning.created_at,
        resolved_at: warning.resolved_at,
    }
}

fn build_dashboard_metrics(
    overview: &DocumentsOverview,
    _ops_state: &OpsLibraryState,
    graph: &GraphSurface,
    attention_count: usize,
) -> Vec<DashboardMetric> {
    // Canonical `in_flight`: `processing + queued` at the document
    // level — exactly what `overview.processingDocuments` is, because
    // that field is built from `LibraryDocumentMetrics` via
    // `build_documents_overview_from_metrics`. Do NOT re-derive from
    // `ops_state.queue_depth + running_attempts` here: those come
    // from `ingest_job` / `ingest_attempt` rows and can legitimately
    // disagree with the document-level bucketing (one document can
    // have multiple jobs/attempts during retries; one queued job can
    // represent a document that still counts as `processing` because
    // a mutation is `running`). The two numbers used to appear on
    // the same response as `metrics[in_flight] = 8756` and
    // `overview.processingDocuments = 8484` for the same moment —
    // that divergence is the drift bug the dashboard release killed
    // for the overview but forgot here. The UI reads either field
    // now without contradiction.
    let in_flight = i64::from(overview.processing_documents);
    let attention = i64::try_from(attention_count).unwrap_or(i64::MAX);

    vec![
        DashboardMetric {
            key: "documents".to_string(),
            label: "Documents".to_string(),
            value: overview.total_documents.to_string(),
            level: MessageLevel::Info,
        },
        DashboardMetric {
            key: "graph_ready".to_string(),
            label: "Graph-ready".to_string(),
            value: graph.graph_ready_document_count.to_string(),
            level: if graph.graph_sparse_document_count > 0 {
                MessageLevel::Warning
            } else {
                MessageLevel::Info
            },
        },
        DashboardMetric {
            key: "in_flight".to_string(),
            label: "In flight".to_string(),
            value: in_flight.to_string(),
            level: if in_flight > 0 { MessageLevel::Warning } else { MessageLevel::Info },
        },
        DashboardMetric {
            key: "attention".to_string(),
            label: "Attention".to_string(),
            value: attention.to_string(),
            level: if attention > 0 { MessageLevel::Error } else { MessageLevel::Info },
        },
    ]
}

fn map_attention_item(warning: &OpsLibraryWarning) -> DashboardAttentionItem {
    let (title, detail, route_path) = match warning.warning_kind.as_str() {
        "stale_vectors" => (
            "Vector rebuild is still running",
            "Some readable documents have not converged onto current vector state yet.",
            "/documents",
        ),
        "stale_relations" => (
            "Graph rebuild is still running",
            "The graph remains behind the readable document set for this library.",
            "/graph",
        ),
        "failed_rebuilds" => (
            "Recent rebuild failed",
            "At least one recent ingestion rebuild failed and needs operator review.",
            "/documents",
        ),
        "bundle_assembly_failures" => (
            "Context bundle assembly failed",
            "Recent bundle assembly failed and downstream graph context may be incomplete.",
            "/graph",
        ),
        _ => (
            "Operator warning",
            "The backend reported a library warning that needs attention.",
            "/documents",
        ),
    };

    DashboardAttentionItem {
        code: warning.warning_kind.clone(),
        title: title.to_string(),
        detail: detail.to_string(),
        route_path: route_path.to_string(),
        level: severity_level(&warning.severity),
    }
}

fn map_operator_warnings(
    warnings: &[OpsLibraryWarning],
    ops_state: &OpsLibraryState,
) -> Vec<OperatorWarning> {
    let mut mapped = warnings
        .iter()
        .map(|warning| OperatorWarning {
            code: warning.warning_kind.clone(),
            level: severity_level(&warning.severity),
            title: humanize_warning_kind(&warning.warning_kind),
            detail: format!(
                "Library {} reported {} at {}.",
                warning.library_id,
                warning.warning_kind.replace('_', " "),
                warning.created_at.to_rfc3339()
            ),
        })
        .collect::<Vec<_>>();

    if ops_state.degraded_state != "healthy" {
        mapped.insert(
            0,
            OperatorWarning {
                code: format!("library_{}", ops_state.degraded_state),
                level: if matches!(
                    ops_state.degraded_state.as_str(),
                    "degraded" | "processing" | "rebuilding"
                ) {
                    MessageLevel::Warning
                } else {
                    MessageLevel::Error
                },
                title: humanize_warning_kind(&format!("library_{}", ops_state.degraded_state)),
                detail: format!(
                    "Queue depth: {}. Running attempts: {}. Failed documents: {}.",
                    ops_state.queue_depth,
                    ops_state.running_attempts,
                    ops_state.failed_document_count
                ),
            },
        );
    }

    mapped
}

fn map_graph_surface(
    summary: &KnowledgeLibrarySummary,
    ops_state: &OpsLibraryState,
    first_warning: Option<&OperatorWarning>,
) -> GraphSurface {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    let readable_without_graph_count =
        summary.document_counts_by_readiness.get("readable").copied().unwrap_or(0);
    let status = if total_documents == 0 {
        GraphStatus::Empty
    } else if ops_state.degraded_state == "rebuilding" || ops_state.running_attempts > 0 {
        if summary.graph_ready_document_count > 0 {
            GraphStatus::Rebuilding
        } else {
            GraphStatus::Building
        }
    } else if summary.graph_ready_document_count > 0
        && summary.graph_sparse_document_count == 0
        && readable_without_graph_count == 0
    {
        GraphStatus::Ready
    } else if summary.graph_ready_document_count > 0
        || summary.graph_sparse_document_count > 0
        || readable_without_graph_count > 0
    {
        GraphStatus::Partial
    } else if ops_state.failed_document_count > 0 {
        GraphStatus::Failed
    } else {
        GraphStatus::Building
    };

    let convergence_status = match status {
        GraphStatus::Ready => Some(GraphConvergenceStatus::Current),
        GraphStatus::Partial | GraphStatus::Building | GraphStatus::Rebuilding => {
            Some(GraphConvergenceStatus::Partial)
        }
        GraphStatus::Failed | GraphStatus::Stale => Some(GraphConvergenceStatus::Degraded),
        GraphStatus::Empty => None,
    };

    GraphSurface {
        library_id: summary.library_id,
        status,
        convergence_status,
        warning: first_warning.map(|warning| warning.detail.clone()),
        node_count: saturating_i32_from_i64(summary.node_count),
        relation_count: saturating_i32_from_i64(summary.edge_count),
        edge_count: saturating_i32_from_i64(summary.edge_count),
        graph_ready_document_count: saturating_i32_from_i64(summary.graph_ready_document_count),
        graph_sparse_document_count: saturating_i32_from_i64(summary.graph_sparse_document_count),
        typed_fact_document_count: saturating_i32_from_i64(summary.typed_fact_document_count),
        updated_at: Some(summary.updated_at),
        nodes: Vec::new(),
        edges: Vec::new(),
        readiness_summary: Some(GraphReadinessSummary {
            library_id: summary.library_id,
            document_counts_by_readiness: summary
                .document_counts_by_readiness
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            graph_ready_document_count: summary.graph_ready_document_count,
            graph_sparse_document_count: summary.graph_sparse_document_count,
            typed_fact_document_count: summary.typed_fact_document_count,
            latest_generation: summary.latest_generation.as_ref().map(|generation| {
                GraphGenerationSummary {
                    generation_id: Some(generation.id),
                    active_graph_generation: 1,
                    degraded_state: Some(ops_state.degraded_state.clone()),
                    updated_at: generation.completed_at.or(Some(generation.created_at)),
                }
            }),
            updated_at: Some(summary.updated_at),
        }),
    }
}

fn map_web_run_summary(summary: ingest::WebIngestRunSummary) -> WebIngestRunSummary {
    WebIngestRunSummary {
        run_id: summary.run_id,
        library_id: summary.library_id,
        mode: summary.mode,
        boundary_policy: summary.boundary_policy,
        max_depth: summary.max_depth,
        max_pages: summary.max_pages,
        ignore_patterns: summary
            .ignore_patterns
            .into_iter()
            .map(|pattern| ironrag_contracts::documents::WebIngestIgnorePattern {
                kind: pattern.kind,
                value: pattern.value,
                source: pattern.source,
            })
            .collect(),
        run_state: map_web_run_state(&summary.run_state),
        seed_url: summary.seed_url,
        counts: WebRunCounts {
            discovered: saturating_i32_from_i64(summary.counts.discovered),
            eligible: saturating_i32_from_i64(summary.counts.eligible),
            processed: saturating_i32_from_i64(summary.counts.processed),
            queued: saturating_i32_from_i64(summary.counts.queued),
            processing: saturating_i32_from_i64(summary.counts.processing),
            duplicates: saturating_i32_from_i64(summary.counts.duplicates),
            excluded: saturating_i32_from_i64(summary.counts.excluded),
            blocked: saturating_i32_from_i64(summary.counts.blocked),
            failed: saturating_i32_from_i64(summary.counts.failed),
            canceled: saturating_i32_from_i64(summary.counts.canceled),
        },
        last_activity_at: summary.last_activity_at,
    }
}

fn severity_level(value: &str) -> MessageLevel {
    match value {
        "error" => MessageLevel::Error,
        "warning" => MessageLevel::Warning,
        _ => MessageLevel::Info,
    }
}

fn map_web_run_state(value: &str) -> WebIngestRunState {
    match value {
        "accepted" => WebIngestRunState::Accepted,
        "discovering" => WebIngestRunState::Discovering,
        "completed" => WebIngestRunState::Completed,
        "completed_partial" => WebIngestRunState::CompletedPartial,
        "failed" => WebIngestRunState::Failed,
        "canceled" => WebIngestRunState::Canceled,
        _ => WebIngestRunState::Processing,
    }
}

fn humanize_warning_kind(value: &str) -> String {
    value
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            chars.next().map_or_else(String::new, |first| {
                format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const fn attention_priority(level: MessageLevel) -> u8 {
    match level {
        MessageLevel::Error => 3,
        MessageLevel::Warning => 2,
        MessageLevel::Info => 1,
    }
}

fn saturating_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn saturating_i32_from_i64(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| if value.is_negative() { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
