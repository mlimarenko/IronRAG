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
        content::{ContentDocumentPipelineState, ContentDocumentSummary},
        ingest,
        knowledge::{KnowledgeLibraryGeneration, KnowledgeLibrarySummary},
        ops::{OpsLibraryState, OpsLibraryWarning},
        runtime_ingestion::RuntimeDocumentActivityStatus,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_USAGE_READ, load_async_operation_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
};
use rustrag_contracts::{
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

async fn get_async_operation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<crate::domains::ops::OpsAsyncOperation>, ApiError> {
    let _ =
        load_async_operation_and_authorize(&auth, &state, operation_id, POLICY_USAGE_READ).await?;
    let operation = state.canonical_services.ops.get_async_operation(&state, operation_id).await?;
    Ok(Json(operation))
}

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

async fn get_library_dashboard(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<DashboardSurface>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_USAGE_READ).await?;
    let (documents, recent_web_runs, knowledge_summary, ops_snapshot, ops_warnings) = tokio::try_join!(
        state.canonical_services.content.list_documents(&state, library_id),
        state.canonical_services.web_ingest.list_runs(&state, library_id),
        state.canonical_services.knowledge.get_library_summary(&state, library_id),
        state.canonical_services.ops.get_library_state_snapshot(&state, library_id),
        state.canonical_services.ops.list_library_warnings(&state, library_id),
    )?;

    let document_summaries = documents.iter().map(map_document_summary).collect::<Vec<_>>();
    let overview = build_documents_overview(&document_summaries);
    let warnings = map_operator_warnings(&ops_warnings, &ops_snapshot.state);
    let graph = map_graph_surface(&knowledge_summary, &ops_snapshot.state, warnings.first());
    let attention = build_attention_items(
        &ops_snapshot.state,
        &ops_warnings,
        &graph,
        document_summaries.as_slice(),
    );
    let metrics = build_dashboard_metrics(&overview, &ops_snapshot.state, &graph, attention.len());
    let recent_documents = sort_recent_documents(document_summaries.clone());

    Ok(Json(DashboardSurface {
        overview,
        metrics,
        recent_documents,
        recent_web_runs: recent_web_runs.into_iter().map(map_web_run_summary).collect(),
        graph,
        attention,
        warnings,
    }))
}

fn sort_recent_documents(mut documents: Vec<DocumentSummary>) -> Vec<DocumentSummary> {
    documents.sort_by(|left, right| {
        right.uploaded_at.cmp(&left.uploaded_at).then_with(|| right.id.cmp(&left.id))
    });
    documents.truncate(6);
    documents
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

fn build_documents_overview(documents: &[DocumentSummary]) -> DocumentsOverview {
    DocumentsOverview {
        total_documents: saturating_i32(documents.len()),
        ready_documents: saturating_i32(
            documents
                .iter()
                .filter(|document| {
                    matches!(document.status, DocumentStatus::Ready | DocumentStatus::ReadyNoGraph)
                })
                .count(),
        ),
        processing_documents: saturating_i32(
            documents
                .iter()
                .filter(|document| {
                    matches!(document.status, DocumentStatus::Queued | DocumentStatus::Processing)
                })
                .count(),
        ),
        failed_documents: saturating_i32(
            documents
                .iter()
                .filter(|document| matches!(document.status, DocumentStatus::Failed))
                .count(),
        ),
        graph_sparse_documents: saturating_i32(
            documents
                .iter()
                .filter(|document| matches!(document.readiness, DocumentReadiness::GraphSparse))
                .count(),
        ),
    }
}

fn build_dashboard_metrics(
    overview: &DocumentsOverview,
    ops_state: &OpsLibraryState,
    graph: &GraphSurface,
    attention_count: usize,
) -> Vec<DashboardMetric> {
    let in_flight = ops_state.queue_depth.saturating_add(ops_state.running_attempts);
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

fn build_attention_items(
    ops_state: &OpsLibraryState,
    warnings: &[OpsLibraryWarning],
    graph: &GraphSurface,
    documents: &[DocumentSummary],
) -> Vec<DashboardAttentionItem> {
    let mut attention = Vec::new();
    let readable_without_graph_count = documents
        .iter()
        .filter(|document| matches!(document.readiness, DocumentReadiness::Readable))
        .count();
    let graph_coverage_gap_count = readable_without_graph_count
        .saturating_add(usize::try_from(graph.graph_sparse_document_count).unwrap_or(usize::MAX));

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
                "{} readable documents still do not contribute to the graph.",
                graph_coverage_gap_count
            ),
            route_path: if readable_without_graph_count > 0 {
                "/documents?readiness=readable".to_string()
            } else {
                "/documents?readiness=graph_sparse".to_string()
            },
            level: MessageLevel::Warning,
        });
    }

    if let Some(document) = documents.iter().find(|document| document.can_retry) {
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

fn map_document_summary(summary: &ContentDocumentSummary) -> DocumentSummary {
    let status = map_document_status(
        &summary.document.document_state,
        summary.readiness.as_ref(),
        summary.readiness_summary.as_ref(),
        &summary.pipeline,
    );
    let readiness = map_document_readiness(
        summary.readiness.as_ref(),
        summary.readiness_summary.as_ref(),
        status,
    );

    DocumentSummary {
        id: summary.document.id,
        workspace_id: Some(summary.document.workspace_id),
        library_id: Some(summary.document.library_id),
        file_name: summary.file_name.clone(),
        file_type: summary
            .active_revision
            .as_ref()
            .map_or_else(|| "unknown".to_string(), |revision| revision.mime_type.clone()),
        file_size: summary.active_revision.as_ref().map_or(0, |revision| revision.byte_size),
        uploaded_at: summary.document.created_at,
        status,
        readiness,
        stage_label: summary
            .pipeline
            .latest_job
            .as_ref()
            .and_then(|job| job.current_stage.clone())
            .or_else(|| {
                summary
                    .readiness_summary
                    .as_ref()
                    .and_then(|details| details.last_job_stage.clone())
            })
            .or_else(|| {
                summary.readiness_summary.as_ref().map(|details| details.preparation_state.clone())
            }),
        progress_percent: None,
        cost_usd: None,
        failure_message: summary
            .readiness_summary
            .as_ref()
            .and_then(|details| details.stalled_reason.clone())
            .or_else(|| {
                summary.pipeline.latest_job.as_ref().and_then(|job| job.failure_code.clone())
            })
            .or_else(|| {
                summary
                    .pipeline
                    .latest_mutation
                    .as_ref()
                    .and_then(|mutation| mutation.failure_code.clone())
            }),
        can_retry: summary
            .pipeline
            .latest_job
            .as_ref()
            .map_or(matches!(status, DocumentStatus::Failed), |job| job.retryable),
        prepared_segment_count: summary
            .prepared_revision
            .as_ref()
            .map(|revision| revision.block_count),
        technical_fact_count: summary
            .prepared_revision
            .as_ref()
            .map(|revision| revision.typed_fact_count),
        source_format: summary
            .prepared_revision
            .as_ref()
            .map(|revision| revision.source_format.clone()),
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

fn map_document_status(
    document_state: &str,
    readiness: Option<&crate::domains::content::ContentRevisionReadiness>,
    readiness_summary: Option<&crate::domains::content::DocumentReadinessSummary>,
    pipeline: &ContentDocumentPipelineState,
) -> DocumentStatus {
    let state = document_state.trim().to_ascii_lowercase();

    if state.contains("failed")
        || pipeline.latest_job.as_ref().and_then(|job| job.failure_code.as_ref()).is_some()
        || pipeline
            .latest_mutation
            .as_ref()
            .and_then(|mutation| mutation.failure_code.as_ref())
            .is_some()
        || readiness_summary
            .as_ref()
            .is_some_and(|summary| summary.readiness_kind.contains("failed"))
        || readiness_summary.as_ref().is_some_and(|summary| {
            matches!(summary.activity_status, RuntimeDocumentActivityStatus::Failed)
        })
    {
        return DocumentStatus::Failed;
    }

    if pipeline.latest_job.as_ref().is_some_and(|job| job.queue_state == "queued")
        || readiness_summary.as_ref().is_some_and(|summary| {
            matches!(summary.activity_status, RuntimeDocumentActivityStatus::Queued)
        })
        || state.contains("queued")
    {
        return DocumentStatus::Queued;
    }

    if pipeline
        .latest_job
        .as_ref()
        .is_some_and(|job| matches!(job.queue_state.as_str(), "leased" | "running" | "processing"))
        || readiness_summary.as_ref().is_some_and(|summary| {
            matches!(
                summary.activity_status,
                RuntimeDocumentActivityStatus::Active
                    | RuntimeDocumentActivityStatus::Retrying
                    | RuntimeDocumentActivityStatus::Blocked
                    | RuntimeDocumentActivityStatus::Stalled
            )
        })
        || state.contains("processing")
        || state.contains("running")
    {
        return DocumentStatus::Processing;
    }

    if readiness_summary
        .as_ref()
        .is_some_and(|summary| summary.graph_coverage_kind.contains("sparse"))
    {
        return DocumentStatus::ReadyNoGraph;
    }

    if readiness_summary
        .as_ref()
        .is_some_and(|summary| summary.graph_coverage_kind.contains("ready"))
        || readiness.as_ref().is_some_and(|readiness| {
            matches!(readiness.graph_state.as_str(), "ready" | "graph_ready")
        })
    {
        return DocumentStatus::Ready;
    }

    if readiness.as_ref().is_some_and(|readiness| {
        matches!(readiness.text_state.as_str(), "readable" | "ready" | "text_readable")
    }) {
        return DocumentStatus::ReadyNoGraph;
    }

    if state.contains("ready") {
        return DocumentStatus::Ready;
    }

    DocumentStatus::Processing
}

fn map_document_readiness(
    readiness: Option<&crate::domains::content::ContentRevisionReadiness>,
    readiness_summary: Option<&crate::domains::content::DocumentReadinessSummary>,
    status: DocumentStatus,
) -> DocumentReadiness {
    if let Some(readiness) = readiness {
        if matches!(readiness.graph_state.as_str(), "ready" | "graph_ready") {
            return DocumentReadiness::GraphReady;
        }
        if matches!(readiness.graph_state.as_str(), "graph_sparse" | "sparse") {
            return DocumentReadiness::GraphSparse;
        }
        if readiness.graph_state == "failed" {
            return DocumentReadiness::Failed;
        }
    }

    if let Some(summary) = readiness_summary {
        if summary.graph_coverage_kind.contains("ready") {
            return DocumentReadiness::GraphReady;
        }
        if summary.graph_coverage_kind.contains("sparse") {
            return DocumentReadiness::GraphSparse;
        }
        if summary.readiness_kind.contains("failed") {
            return DocumentReadiness::Failed;
        }
    }

    match status {
        DocumentStatus::ReadyNoGraph => return DocumentReadiness::Readable,
        DocumentStatus::Ready => return DocumentReadiness::GraphReady,
        DocumentStatus::Failed => return DocumentReadiness::Failed,
        DocumentStatus::Queued | DocumentStatus::Processing => {}
    }

    if let Some(readiness) = readiness {
        if matches!(readiness.text_state.as_str(), "readable" | "ready" | "text_readable") {
            return DocumentReadiness::Readable;
        }
        if matches!(readiness.text_state.as_str(), "queued" | "processing") {
            return DocumentReadiness::Processing;
        }
        if readiness.text_state == "failed" {
            return DocumentReadiness::Failed;
        }
    }

    match status {
        DocumentStatus::Ready => DocumentReadiness::GraphReady,
        DocumentStatus::ReadyNoGraph => DocumentReadiness::Readable,
        DocumentStatus::Queued | DocumentStatus::Processing => DocumentReadiness::Processing,
        DocumentStatus::Failed => DocumentReadiness::Failed,
    }
}

fn severity_level(value: &str) -> MessageLevel {
    match value {
        "error" => MessageLevel::Error,
        "warning" => MessageLevel::Warning,
        _ => MessageLevel::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::{map_document_readiness, map_document_summary};
    use crate::domains::{
        content::{
            ContentDocument, ContentDocumentPipelineState, ContentDocumentSummary,
            ContentRevisionReadiness, DocumentReadinessSummary,
        },
        ops::OpsAsyncOperation,
        runtime_ingestion::RuntimeDocumentActivityStatus,
    };
    use chrono::Utc;
    use rustrag_contracts::documents::{DocumentReadiness, DocumentStatus};
    use serde_json::json;
    use uuid::Uuid;

    fn sample_summary() -> ContentDocumentSummary {
        ContentDocumentSummary {
            document: ContentDocument {
                id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                external_key: "legacy-external-key".to_string(),
                document_state: "active".to_string(),
                created_at: Utc::now(),
            },
            file_name: "canonical-file-name.txt".to_string(),
            head: None,
            active_revision: None,
            source_access: None,
            readiness: None,
            readiness_summary: None,
            prepared_revision: None,
            web_page_provenance: None,
            pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
        }
    }

    #[test]
    fn ready_no_graph_status_without_sparse_coverage_maps_to_readable() {
        let readiness = ContentRevisionReadiness {
            revision_id: Uuid::now_v7(),
            text_state: "ready".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "accepted".to_string(),
            text_readable_at: Some(Utc::now()),
            vector_ready_at: Some(Utc::now()),
            graph_ready_at: None,
        };

        let mapped = map_document_readiness(Some(&readiness), None, DocumentStatus::ReadyNoGraph);
        assert_eq!(mapped, DocumentReadiness::Readable);
    }

    #[test]
    fn sparse_coverage_summary_still_maps_to_graph_sparse() {
        let summary = DocumentReadinessSummary {
            document_id: Uuid::now_v7(),
            active_revision_id: Some(Uuid::now_v7()),
            readiness_kind: "readable".to_string(),
            activity_status: RuntimeDocumentActivityStatus::Ready,
            stalled_reason: None,
            preparation_state: "ready".to_string(),
            graph_coverage_kind: "graph_sparse".to_string(),
            typed_fact_coverage: Some(1.0),
            last_mutation_id: None,
            last_job_stage: None,
            updated_at: Utc::now(),
        };

        let mapped = map_document_readiness(None, Some(&summary), DocumentStatus::ReadyNoGraph);
        assert_eq!(mapped, DocumentReadiness::GraphSparse);
    }

    #[test]
    fn document_summary_uses_canonical_summary_file_name() {
        let mut summary = sample_summary();
        summary.document.external_key = "stale-fallback".to_string();

        let mapped = map_document_summary(&summary);

        assert_eq!(mapped.file_name, "canonical-file-name.txt");
    }

    #[test]
    fn async_operation_serializes_using_canonical_camel_case_fields() {
        let operation = OpsAsyncOperation {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Some(Uuid::now_v7()),
            operation_kind: "content_mutation".to_string(),
            status: "ready".to_string(),
            surface_kind: Some("rest".to_string()),
            subject_kind: Some("content_mutation".to_string()),
            subject_id: Some(Uuid::now_v7()),
            failure_code: None,
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
        };

        let serialized =
            serde_json::to_value(&operation).expect("ops async operation to serialize");

        assert!(serialized.get("completedAt").is_some());
        assert!(serialized.get("completed_at").is_none());
        assert_eq!(serialized.get("status"), Some(&json!("ready")));
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
