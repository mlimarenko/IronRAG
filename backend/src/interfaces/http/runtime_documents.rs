use axum::{
    Json, Router,
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use futures::{StreamExt, stream};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::usage_governance::{RuntimeStageBillingPolicy, runtime_stage_billing_policy},
    infra::repositories::{
        self, AttemptStageAccountingRow, DocumentRevisionRow, IngestionExecutionPayload,
        IngestionJobRow, LogicalDocumentProjectionRow, RuntimeExtractedContentRow,
        RuntimeIngestionRunRow, RuntimeIngestionStageEventRow,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE},
        router_support::{
            ApiError, ApiWarningBody, blocked_activity_warning, map_runtime_lifecycle_error,
            partial_accounting_warning, stalled_activity_warning,
        },
        runtime_support::load_library_and_authorize,
    },
    services::document_reconciliation::{
        AppendDocumentRequest, ReplaceDocumentRequest, queue_append_document_mutation,
        queue_replace_document_mutation,
    },
    services::runtime_ingestion::{
        QueueRuntimeUploadRequest, RuntimeUploadFileInput, classify_runtime_document_activity,
        delete_runtime_run_and_rebuild, queue_new_runtime_upload,
        reprocess_runtime_run_and_rebuild, requeue_runtime_run,
    },
};

#[derive(Debug, Deserialize)]
struct RuntimeDocumentsQuery {
    status: Option<String>,
    #[serde(rename = "fileType")]
    file_type: Option<String>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendDocumentPayload {
    content: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadAcceptedResponse {
    upload_batch_id: Uuid,
    accepted: Vec<RuntimeDocumentRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDocumentSurfaceResponse {
    summary: RuntimeDocumentSummary,
    warnings: Vec<ApiWarningBody>,
    rows: Vec<RuntimeDocumentRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDocumentSummary {
    queued: usize,
    processing: usize,
    ready: usize,
    ready_no_graph: usize,
    failed: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDocumentRow {
    id: Uuid,
    track_id: String,
    file_name: String,
    file_type: String,
    status: String,
    stage: String,
    progress_percent: Option<i32>,
    activity_status: String,
    last_activity_at: Option<String>,
    stalled_reason: Option<String>,
    chunk_count: Option<usize>,
    graph_node_count: Option<usize>,
    graph_edge_count: Option<usize>,
    latest_error: Option<String>,
    active_revision_no: Option<i32>,
    latest_attempt_no: i32,
    total_cost: Option<f64>,
    currency: Option<String>,
    accounting_status: String,
    partial_history: bool,
    partial_history_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDocumentDetailResponse {
    #[serde(flatten)]
    row: RuntimeDocumentRow,
    warnings: Vec<ApiWarningBody>,
    extraction: Option<RuntimeExtractionDetail>,
    graph: RuntimeGraphContributionSummary,
    stage_history: Vec<RuntimeStageEventResponse>,
    revisions: Vec<RuntimeRevisionResponse>,
    attempts: Vec<RuntimeAttemptResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeExtractionDetail {
    extraction_kind: String,
    page_count: Option<i32>,
    char_count: Option<i32>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeGraphContributionSummary {
    node_count: usize,
    edge_count: usize,
    evidence_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStageEventResponse {
    stage: String,
    status: String,
    message: Option<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRevisionResponse {
    id: Uuid,
    revision_no: i32,
    revision_kind: String,
    status: String,
    source_file_name: String,
    accepted_at: DateTime<Utc>,
    activated_at: Option<DateTime<Utc>>,
    superseded_at: Option<DateTime<Utc>>,
    is_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeAttemptResponse {
    attempt_no: i32,
    revision_no: Option<i32>,
    revision_id: Option<Uuid>,
    attempt_kind: Option<String>,
    status: String,
    activity_status: String,
    last_activity_at: Option<String>,
    chunk_count: Option<usize>,
    graph_node_count: Option<usize>,
    graph_edge_count: Option<usize>,
    queue_elapsed_ms: Option<i64>,
    total_elapsed_ms: Option<i64>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    partial_history: bool,
    partial_history_reason: Option<String>,
    cost_summary: RuntimeAttemptCostSummaryResponse,
    stage_benchmarks: Vec<RuntimeStageBenchmarkResponse>,
    warnings: Vec<ApiWarningBody>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeAttemptCostSummaryResponse {
    total_estimated_cost: Option<f64>,
    currency: Option<String>,
    priced_stage_count: i32,
    unpriced_stage_count: i32,
    accounting_status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStageBenchmarkResponse {
    stage: String,
    status: String,
    message: Option<String>,
    provider_kind: Option<String>,
    model_name: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    elapsed_ms: Option<i64>,
    accounting: Option<RuntimeStageAccountingResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStageAccountingResponse {
    pricing_status: String,
    usage_event_id: Option<Uuid>,
    cost_ledger_id: Option<Uuid>,
    pricing_catalog_entry_id: Option<Uuid>,
    estimated_cost: Option<f64>,
    currency: Option<String>,
    attribution_source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeMutationAccepted {
    accepted: bool,
    operation: String,
    track_id: Option<String>,
    revision_id: Option<Uuid>,
    mutation_id: Option<Uuid>,
    attempt_no: Option<i32>,
}

#[derive(Debug, Clone)]
struct ResolvedContributionSummary {
    chunk_count: Option<usize>,
    graph_node_count: Option<usize>,
    graph_edge_count: Option<usize>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route(
            "/runtime/libraries/{library_id}/documents",
            axum::routing::post(upload_runtime_documents).get(list_runtime_documents),
        )
        .route(
            "/runtime/libraries/{library_id}/documents/{document_id}",
            axum::routing::get(get_runtime_document).delete(delete_runtime_document),
        )
        .route(
            "/runtime/libraries/{library_id}/documents/{document_id}/append",
            axum::routing::post(append_runtime_document),
        )
        .route(
            "/runtime/libraries/{library_id}/documents/{document_id}/replace",
            axum::routing::post(replace_runtime_document),
        )
        .route(
            "/runtime/libraries/{library_id}/documents/{document_id}/retry",
            axum::routing::post(retry_runtime_document),
        )
        .route(
            "/runtime/libraries/{library_id}/documents/{document_id}/reprocess",
            axum::routing::post(reprocess_runtime_document),
        )
}

async fn upload_runtime_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadAcceptedResponse>), ApiError> {
    let project =
        load_library_and_authorize(&auth, &state, library_id, POLICY_DOCUMENTS_WRITE).await?;
    let upload_batch_id = Uuid::now_v7();
    let upload_limit_bytes = state.ui_runtime.upload_max_size_mb.saturating_mul(1024 * 1024);
    let mut requests = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("invalid multipart payload".into()))?
    {
        let Some(name) = field.name() else {
            continue;
        };
        if name != "file" && name != "files" {
            continue;
        }

        let file_name = field
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));
        let mime_type = field.content_type().map(str::to_string);
        let file_bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::BadRequest("invalid file body".into()))?
            .to_vec();
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        if file_size_bytes > upload_limit_bytes {
            return Err(ApiError::BadRequest(format!(
                "file {} exceeds the {} MB upload limit",
                file_name, state.ui_runtime.upload_max_size_mb
            )));
        }
        requests.push(QueueRuntimeUploadRequest {
            project_id: project.id,
            upload_batch_id: Some(upload_batch_id),
            requested_by: Some(auth.token_id.to_string()),
            trigger_kind: "runtime_upload".to_string(),
            parent_job_id: None,
            idempotency_key: None,
            file: RuntimeUploadFileInput {
                source_id: None,
                file_name,
                mime_type,
                file_bytes,
                title: None,
            },
        });
    }

    if requests.is_empty() {
        return Err(ApiError::BadRequest("no files were uploaded".into()));
    }

    let upload_concurrency = state.settings.ingestion_worker_concurrency.max(1);
    let queued_results = stream::iter(requests.into_iter().map(|request| {
        let state = state.clone();
        async move { queue_new_runtime_upload(&state, request).await }
    }))
    .buffered(upload_concurrency)
    .collect::<Vec<_>>()
    .await;
    let mut accepted = Vec::with_capacity(queued_results.len());
    for queued in queued_results {
        let queued = queued.map_err(|error| ApiError::BadRequest(error.to_string()))?;
        accepted.push(load_runtime_document_row(&state, &queued.runtime_run).await?);
    }

    Ok((StatusCode::ACCEPTED, Json(UploadAcceptedResponse { upload_batch_id, accepted })))
}

async fn list_runtime_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<RuntimeDocumentsQuery>,
) -> Result<Json<RuntimeDocumentSurfaceResponse>, ApiError> {
    let project =
        load_library_and_authorize(&auth, &state, library_id, POLICY_DOCUMENTS_READ).await?;
    let rows = repositories::list_runtime_ingestion_runs_by_project(
        &state.persistence.postgres,
        project.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let rows =
        rows.into_iter().filter(|row| matches_runtime_filters(row, &query)).collect::<Vec<_>>();
    let summary = build_runtime_document_summary(&rows);
    let mut runtime_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        runtime_rows.push(load_runtime_document_row(&state, row).await?);
    }

    Ok(Json(RuntimeDocumentSurfaceResponse {
        summary,
        warnings: surface_warnings(&runtime_rows),
        rows: runtime_rows,
    }))
}

async fn get_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RuntimeDocumentDetailResponse>, ApiError> {
    let runtime_run =
        load_runtime_run_for_library(&auth, &state, library_id, document_id, POLICY_DOCUMENTS_READ)
            .await?;
    let stage_events =
        repositories::list_runtime_stage_events_by_run(&state.persistence.postgres, runtime_run.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let stage_history = stage_events
        .iter()
        .cloned()
        .map(|row| RuntimeStageEventResponse {
            stage: row.stage,
            status: row.status,
            message: row.message,
            timestamp: row.created_at,
        })
        .collect::<Vec<_>>();
    let extraction = repositories::get_runtime_extracted_content_by_run(
        &state.persistence.postgres,
        runtime_run.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .map(map_runtime_extraction_detail);
    let graph = match (runtime_run.document_id, runtime_run.revision_id) {
        (Some(document_id), Some(revision_id)) => {
            repositories::count_runtime_graph_contributions_by_document_revision(
                &state.persistence.postgres,
                runtime_run.project_id,
                document_id,
                revision_id,
            )
            .await
            .map(|counts| RuntimeGraphContributionSummary {
                node_count: usize::try_from(counts.node_count).unwrap_or_default(),
                edge_count: usize::try_from(counts.edge_count).unwrap_or_default(),
                evidence_count: usize::try_from(counts.evidence_count).unwrap_or_default(),
            })
            .map_err(|_| ApiError::Internal)?
        }
        (Some(document_id), None) => repositories::count_runtime_graph_contributions_by_document(
            &state.persistence.postgres,
            runtime_run.project_id,
            document_id,
        )
        .await
        .map(|counts| RuntimeGraphContributionSummary {
            node_count: usize::try_from(counts.node_count).unwrap_or_default(),
            edge_count: usize::try_from(counts.edge_count).unwrap_or_default(),
            evidence_count: usize::try_from(counts.evidence_count).unwrap_or_default(),
        })
        .map_err(|_| ApiError::Internal)?,
        (None, _) => {
            RuntimeGraphContributionSummary { node_count: 0, edge_count: 0, evidence_count: 0 }
        }
    };
    let projection = load_logical_projection(&state, runtime_run.document_id).await?;
    let revisions = load_document_revisions(&state, runtime_run.document_id).await?;
    let stage_accounting = repositories::list_attempt_stage_accounting_by_run(
        &state.persistence.postgres,
        runtime_run.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let jobs = repositories::list_ingestion_jobs_by_runtime_ingestion_run_id(
        &state.persistence.postgres,
        runtime_run.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let contribution = load_runtime_document_contribution_summary(
        &state,
        runtime_run.project_id,
        runtime_run.document_id,
        runtime_run.revision_id,
    )
    .await?;
    let current_row = load_runtime_document_row(&state, &runtime_run).await?;
    let attempts = build_runtime_attempts(
        &runtime_run,
        &revisions,
        &stage_events,
        &stage_accounting,
        &jobs,
        &contribution,
        &current_row.activity_status,
    );

    Ok(Json(RuntimeDocumentDetailResponse {
        row: current_row,
        warnings: detail_warnings(&runtime_run, &attempts),
        extraction,
        graph,
        stage_history,
        revisions: revisions
            .iter()
            .map(|revision| map_revision_response(revision, projection.as_ref()))
            .collect(),
        attempts,
    }))
}

async fn retry_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<RuntimeMutationAccepted>), ApiError> {
    let runtime_run = load_runtime_run_for_library(
        &auth,
        &state,
        library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    if runtime_run.status != "failed" {
        return Err(ApiError::BadRequest("document is not in failed state".into()));
    }
    let requested_by = auth.token_id.to_string();

    let (runtime_run, _) =
        requeue_runtime_run(&state, &runtime_run, Some(&requested_by), "runtime_retry", None)
            .await
            .map_err(|_| ApiError::Internal)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(RuntimeMutationAccepted {
            accepted: true,
            operation: "retry".to_string(),
            track_id: Some(runtime_run.track_id),
            revision_id: runtime_run.revision_id,
            mutation_id: None,
            attempt_no: Some(runtime_run.current_attempt_no),
        }),
    ))
}

async fn reprocess_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<RuntimeMutationAccepted>), ApiError> {
    let runtime_run = load_runtime_run_for_library(
        &auth,
        &state,
        library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    if matches!(runtime_run.status.as_str(), "queued" | "processing") {
        return Err(ApiError::BadRequest("document is still processing".into()));
    }
    if let Some(existing_document_id) = runtime_run.document_id {
        repositories::delete_document_by_id(&state.persistence.postgres, existing_document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    let requested_by = auth.token_id.to_string();

    let (runtime_run, _) = reprocess_runtime_run_and_rebuild(
        &state,
        &runtime_run,
        Some(&requested_by),
        "runtime_reprocess",
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(RuntimeMutationAccepted {
            accepted: true,
            operation: "reprocess".to_string(),
            track_id: Some(runtime_run.track_id),
            revision_id: runtime_run.revision_id,
            mutation_id: None,
            attempt_no: Some(runtime_run.current_attempt_no),
        }),
    ))
}

async fn append_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<AppendDocumentPayload>,
) -> Result<(StatusCode, Json<RuntimeMutationAccepted>), ApiError> {
    let runtime_run = load_runtime_run_for_library(
        &auth,
        &state,
        library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let mutation = queue_append_document_mutation(
        &state,
        AppendDocumentRequest {
            runtime_run,
            requested_by: Some(auth.token_id.to_string()),
            trigger_kind: "runtime_append".to_string(),
            parent_job_id: None,
            appended_text: payload.content,
        },
    )
    .await
    .map_err(map_runtime_lifecycle_error)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(RuntimeMutationAccepted {
            accepted: true,
            operation: "append".to_string(),
            track_id: Some(mutation.runtime_run.track_id),
            revision_id: Some(mutation.target_revision.id),
            mutation_id: Some(mutation.mutation_workflow.id),
            attempt_no: Some(mutation.runtime_run.current_attempt_no),
        }),
    ))
}

async fn replace_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<RuntimeMutationAccepted>), ApiError> {
    let runtime_run = load_runtime_run_for_library(
        &auth,
        &state,
        library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let upload_limit_bytes = state.ui_runtime.upload_max_size_mb.saturating_mul(1024 * 1024);
    let mut replacement_file = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("invalid multipart payload".into()))?
    {
        let Some(name) = field.name() else {
            continue;
        };
        if name != "file" && name != "files" {
            continue;
        }

        let file_name = field
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("replace-{}", Uuid::now_v7()));
        let mime_type = field.content_type().map(str::to_string);
        let file_bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::BadRequest("invalid file body".into()))?
            .to_vec();
        let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
        if file_size_bytes > upload_limit_bytes {
            return Err(ApiError::BadRequest(format!(
                "file {} exceeds the {} MB upload limit",
                file_name, state.ui_runtime.upload_max_size_mb
            )));
        }
        replacement_file = Some(RuntimeUploadFileInput {
            source_id: None,
            file_name,
            mime_type,
            file_bytes,
            title: None,
        });
        break;
    }

    let file = replacement_file
        .ok_or_else(|| ApiError::BadRequest("no replacement file was uploaded".into()))?;
    let mutation = queue_replace_document_mutation(
        &state,
        ReplaceDocumentRequest {
            runtime_run,
            requested_by: Some(auth.token_id.to_string()),
            trigger_kind: "runtime_replace".to_string(),
            parent_job_id: None,
            file,
        },
    )
    .await
    .map_err(map_runtime_lifecycle_error)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(RuntimeMutationAccepted {
            accepted: true,
            operation: "replace".to_string(),
            track_id: Some(mutation.runtime_run.track_id),
            revision_id: Some(mutation.target_revision.id),
            mutation_id: Some(mutation.mutation_workflow.id),
            attempt_no: Some(mutation.runtime_run.current_attempt_no),
        }),
    ))
}

async fn delete_runtime_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<RuntimeMutationAccepted>), ApiError> {
    let runtime_run = load_runtime_run_for_library(
        &auth,
        &state,
        library_id,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let requested_by = auth.token_id.to_string();
    let mutation = delete_runtime_run_and_rebuild(&state, &runtime_run, Some(&requested_by))
        .await
        .map_err(map_runtime_lifecycle_error)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(RuntimeMutationAccepted {
            accepted: true,
            operation: "delete".to_string(),
            track_id: Some(runtime_run.track_id),
            revision_id: runtime_run.revision_id,
            mutation_id: mutation.as_ref().map(|row| row.id),
            attempt_no: Some(runtime_run.current_attempt_no),
        }),
    ))
}

async fn load_runtime_run_for_library(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    document_id: Uuid,
    policy: &[&str],
) -> Result<RuntimeIngestionRunRow, ApiError> {
    let project = load_library_and_authorize(auth, state, library_id, policy).await?;
    let runtime_run =
        repositories::get_runtime_ingestion_run_by_id(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("runtime document {document_id} not found"))
            })?;
    if runtime_run.project_id != project.id {
        return Err(ApiError::NotFound(format!("runtime document {document_id} not found")));
    }

    Ok(runtime_run)
}

fn matches_runtime_filters(row: &RuntimeIngestionRunRow, query: &RuntimeDocumentsQuery) -> bool {
    if let Some(status) = query.status.as_deref() {
        if row.status != status {
            return false;
        }
    }
    if let Some(file_type) = query.file_type.as_deref() {
        if row.file_type != file_type.to_ascii_lowercase() && row.file_type != file_type {
            return false;
        }
    }
    if let Some(q) = query.q.as_deref() {
        let normalized_query = q.trim().to_ascii_lowercase();
        if !normalized_query.is_empty()
            && !row.file_name.to_ascii_lowercase().contains(&normalized_query)
            && !row.track_id.to_ascii_lowercase().contains(&normalized_query)
        {
            return false;
        }
    }

    true
}

fn build_runtime_document_summary(rows: &[RuntimeIngestionRunRow]) -> RuntimeDocumentSummary {
    let mut summary =
        RuntimeDocumentSummary { queued: 0, processing: 0, ready: 0, ready_no_graph: 0, failed: 0 };
    for row in rows {
        match row.status.as_str() {
            "queued" => summary.queued += 1,
            "processing" => summary.processing += 1,
            "ready" => summary.ready += 1,
            "ready_no_graph" => summary.ready_no_graph += 1,
            "failed" => summary.failed += 1,
            _ => {}
        }
    }
    summary
}

async fn load_runtime_document_row(
    state: &AppState,
    row: &RuntimeIngestionRunRow,
) -> Result<RuntimeDocumentRow, ApiError> {
    let projection = load_logical_projection(state, row.document_id).await?;
    let contribution = load_runtime_document_contribution_summary(
        state,
        row.project_id,
        row.document_id,
        row.revision_id,
    )
    .await?;
    let activity = classify_runtime_document_activity(state, row);
    let latest_summary =
        repositories::get_attempt_stage_cost_summary_by_run(&state.persistence.postgres, row.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let partial_history = matches!(row.status.as_str(), "ready" | "ready_no_graph" | "failed")
        && (row.queue_elapsed_ms.is_none()
            || (row.finished_at.is_some() && row.total_elapsed_ms.is_none())
            || latest_summary.is_none());
    Ok(RuntimeDocumentRow {
        id: row.id,
        track_id: row.track_id.clone(),
        file_name: row.file_name.clone(),
        file_type: row.file_type.clone(),
        status: row.status.clone(),
        stage: row.current_stage.clone(),
        progress_percent: row.progress_percent,
        activity_status: activity.activity_status,
        last_activity_at: activity.last_activity_at.map(|value| value.to_rfc3339()),
        stalled_reason: activity.stalled_reason,
        chunk_count: contribution.chunk_count,
        graph_node_count: contribution.graph_node_count,
        graph_edge_count: contribution.graph_edge_count,
        latest_error: row.latest_error_message.clone(),
        active_revision_no: projection.as_ref().and_then(|item| item.active_revision_no),
        latest_attempt_no: row.current_attempt_no,
        total_cost: latest_summary
            .as_ref()
            .and_then(|item| decimal_to_f64(item.total_estimated_cost)),
        currency: latest_summary.as_ref().and_then(|item| item.currency.clone()),
        accounting_status: latest_summary
            .as_ref()
            .map(|item| item.accounting_status.clone())
            .unwrap_or_else(|| "unpriced".to_string()),
        partial_history,
        partial_history_reason: partial_history
            .then_some("Legacy runtime history is incomplete for this attempt.".to_string()),
    })
}

fn map_runtime_extraction_detail(row: RuntimeExtractedContentRow) -> RuntimeExtractionDetail {
    RuntimeExtractionDetail {
        extraction_kind: row.extraction_kind,
        page_count: row.page_count,
        char_count: row.char_count,
        warnings: serde_json::from_value(row.extraction_warnings_json).unwrap_or_default(),
    }
}

async fn load_logical_projection(
    state: &AppState,
    document_id: Option<Uuid>,
) -> Result<Option<LogicalDocumentProjectionRow>, ApiError> {
    match document_id {
        Some(document_id) => repositories::get_logical_document_projection_by_id(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal),
        None => Ok(None),
    }
}

async fn load_document_revisions(
    state: &AppState,
    document_id: Option<Uuid>,
) -> Result<Vec<DocumentRevisionRow>, ApiError> {
    match document_id {
        Some(document_id) => repositories::list_document_revisions_by_document_id(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal),
        None => Ok(Vec::new()),
    }
}

fn map_revision_response(
    revision: &DocumentRevisionRow,
    projection: Option<&LogicalDocumentProjectionRow>,
) -> RuntimeRevisionResponse {
    RuntimeRevisionResponse {
        id: revision.id,
        revision_no: revision.revision_no,
        revision_kind: revision.revision_kind.clone(),
        status: revision.status.clone(),
        source_file_name: revision.source_file_name.clone(),
        accepted_at: revision.accepted_at,
        activated_at: revision.activated_at,
        superseded_at: revision.superseded_at,
        is_active: projection
            .and_then(|item| item.current_revision_id)
            .is_some_and(|current_id| current_id == revision.id),
    }
}

fn build_runtime_attempts(
    runtime_run: &RuntimeIngestionRunRow,
    revisions: &[DocumentRevisionRow],
    stage_events: &[RuntimeIngestionStageEventRow],
    stage_accounting: &[AttemptStageAccountingRow],
    jobs: &[IngestionJobRow],
    current_contribution: &ResolvedContributionSummary,
    current_activity_status: &str,
) -> Vec<RuntimeAttemptResponse> {
    let mut attempt_nos = stage_events.iter().map(|item| item.attempt_no).collect::<BTreeSet<_>>();
    attempt_nos.insert(runtime_run.current_attempt_no);
    let attempt_nos = attempt_nos.into_iter().collect::<Vec<_>>();
    let revision_no_by_id = revisions
        .iter()
        .map(|revision| (revision.id, revision.revision_no))
        .collect::<HashMap<_, _>>();
    let initial_revision_no = revisions.iter().map(|revision| revision.revision_no).min();
    let payload_by_attempt = map_attempt_payloads(&attempt_nos, jobs);
    let job_by_attempt = map_attempt_jobs(&attempt_nos, jobs);
    let stage_events_by_attempt = group_stage_events_by_attempt(stage_events);
    let accounting_by_event = stage_accounting
        .iter()
        .cloned()
        .map(|row| (row.stage_event_id, row))
        .collect::<HashMap<_, _>>();

    attempt_nos
        .into_iter()
        .rev()
        .map(|attempt_no| {
            let attempt_stage_events =
                stage_events_by_attempt.get(&attempt_no).cloned().unwrap_or_default();
            let payload = payload_by_attempt.get(&attempt_no);
            let job = job_by_attempt.get(&attempt_no);
            let revision_no = payload
                .and_then(|item| item.target_revision_id)
                .and_then(|revision_id| revision_no_by_id.get(&revision_id).copied())
                .or_else(|| {
                    payload
                        .and_then(|item| item.attempt_kind.as_deref())
                        .filter(|kind| *kind == "initial_upload")
                        .and(initial_revision_no)
                });
            let stage_benchmarks = attempt_stage_events
                .iter()
                .map(|event| RuntimeStageBenchmarkResponse {
                    stage: event.stage.clone(),
                    status: event.status.clone(),
                    message: event.message.clone(),
                    provider_kind: event.provider_kind.clone(),
                    model_name: event.model_name.clone(),
                    started_at: event.started_at,
                    finished_at: event.finished_at,
                    elapsed_ms: event.elapsed_ms,
                    accounting: accounting_by_event.get(&event.id).and_then(|row| {
                        stage_accounting_belongs_to_billable_stage(row, &event.stage)
                            .then(|| map_stage_accounting_response(row, event))
                    }),
                })
                .collect::<Vec<_>>();
            let cost_summary =
                summarize_attempt_accounting(&attempt_stage_events, stage_accounting);
            let started_at = attempt_started_at(&attempt_stage_events).or(
                if attempt_no == runtime_run.current_attempt_no {
                    runtime_run.started_at
                } else {
                    None
                },
            );
            let finished_at = attempt_finished_at(&attempt_stage_events).or(
                if attempt_no == runtime_run.current_attempt_no {
                    runtime_run.finished_at
                } else {
                    None
                },
            );
            let queue_started_at = attempt_queue_started_at(&attempt_stage_events)
                .unwrap_or(runtime_run.queue_started_at);
            let queue_elapsed_ms = if attempt_no == runtime_run.current_attempt_no {
                runtime_run.queue_elapsed_ms.or_else(|| {
                    started_at.map(|started_at| {
                        started_at.signed_duration_since(queue_started_at).num_milliseconds().max(0)
                    })
                })
            } else {
                started_at.map(|started_at| {
                    started_at.signed_duration_since(queue_started_at).num_milliseconds().max(0)
                })
            };
            let total_elapsed_ms = if attempt_no == runtime_run.current_attempt_no {
                runtime_run.total_elapsed_ms.or_else(|| {
                    finished_at.map(|finished_at| {
                        finished_at
                            .signed_duration_since(queue_started_at)
                            .num_milliseconds()
                            .max(0)
                    })
                })
            } else {
                finished_at.map(|finished_at| {
                    finished_at.signed_duration_since(queue_started_at).num_milliseconds().max(0)
                })
            };
            let partial_history_reason = attempt_partial_history_reason(
                attempt_no,
                runtime_run,
                &attempt_stage_events,
                payload,
            );
            let attempt_status = attempt_status(runtime_run, job.copied(), &attempt_stage_events);
            let attempt_activity_status = attempt_activity_status(
                runtime_run,
                attempt_no,
                &attempt_status,
                current_activity_status,
            )
            .to_string();
            let attempt_last_activity_at =
                attempt_last_activity_at(runtime_run, attempt_no, &attempt_stage_events)
                    .map(|value| value.to_rfc3339());
            let warnings =
                attempt_warnings(&attempt_activity_status, &cost_summary.accounting_status);
            RuntimeAttemptResponse {
                attempt_no,
                revision_no,
                revision_id: payload.and_then(|item| item.target_revision_id),
                attempt_kind: payload.and_then(|item| item.attempt_kind.clone()),
                status: attempt_status.clone(),
                activity_status: attempt_activity_status.clone(),
                last_activity_at: attempt_last_activity_at,
                chunk_count: if attempt_no == runtime_run.current_attempt_no {
                    current_contribution.chunk_count
                } else {
                    None
                },
                graph_node_count: if attempt_no == runtime_run.current_attempt_no {
                    current_contribution.graph_node_count
                } else {
                    None
                },
                graph_edge_count: if attempt_no == runtime_run.current_attempt_no {
                    current_contribution.graph_edge_count
                } else {
                    None
                },
                queue_elapsed_ms,
                total_elapsed_ms,
                started_at,
                finished_at,
                partial_history: partial_history_reason.is_some(),
                partial_history_reason,
                cost_summary,
                stage_benchmarks,
                warnings,
            }
        })
        .collect()
}

fn summarize_attempt_accounting(
    attempt_stage_events: &[RuntimeIngestionStageEventRow],
    stage_accounting: &[AttemptStageAccountingRow],
) -> RuntimeAttemptCostSummaryResponse {
    let stage_event_ids =
        attempt_stage_events.iter().map(|event| event.id).collect::<BTreeSet<_>>();
    let attempt_rows = stage_accounting
        .iter()
        .filter(|row| {
            stage_event_ids.contains(&row.stage_event_id)
                && attempt_stage_events.iter().any(|event| {
                    event.id == row.stage_event_id
                        && stage_accounting_belongs_to_billable_stage(row, &event.stage)
                })
        })
        .collect::<Vec<_>>();
    let total_estimated_cost = attempt_rows
        .iter()
        .filter_map(|row| row.estimated_cost)
        .fold(rust_decimal::Decimal::ZERO, |acc, value| acc + value);
    let priced_stage_count =
        i32::try_from(attempt_rows.iter().filter(|row| row.pricing_status == "priced").count())
            .unwrap_or(i32::MAX);
    let unpriced_stage_count =
        i32::try_from(attempt_rows.iter().filter(|row| row.pricing_status != "priced").count())
            .unwrap_or(i32::MAX);
    RuntimeAttemptCostSummaryResponse {
        total_estimated_cost: if attempt_rows.iter().any(|row| row.estimated_cost.is_some()) {
            decimal_to_f64(Some(total_estimated_cost))
        } else {
            None
        },
        currency: attempt_rows.iter().find_map(|row| row.currency.clone()),
        priced_stage_count,
        unpriced_stage_count,
        accounting_status: if priced_stage_count > 0 && unpriced_stage_count == 0 {
            "priced".to_string()
        } else if priced_stage_count > 0 {
            "partial".to_string()
        } else {
            "unpriced".to_string()
        },
    }
}

fn map_stage_accounting_response(
    row: &AttemptStageAccountingRow,
    event: &RuntimeIngestionStageEventRow,
) -> RuntimeStageAccountingResponse {
    RuntimeStageAccountingResponse {
        pricing_status: row.pricing_status.clone(),
        usage_event_id: row.usage_event_id,
        cost_ledger_id: row.cost_ledger_id,
        pricing_catalog_entry_id: row.pricing_catalog_entry_id,
        estimated_cost: decimal_to_f64(row.estimated_cost),
        currency: row.currency.clone(),
        attribution_source: stage_attribution_source(row, &event.stage).to_string(),
    }
}

fn stage_accounting_belongs_to_billable_stage(
    row: &AttemptStageAccountingRow,
    event_stage: &str,
) -> bool {
    if row.stage != event_stage {
        return false;
    }
    match runtime_stage_billing_policy(event_stage) {
        RuntimeStageBillingPolicy::Billable { capability, billing_unit } => {
            row.capability == pricing_capability_label(&capability)
                && row.billing_unit == pricing_billing_unit_label(&billing_unit)
        }
        RuntimeStageBillingPolicy::NonBillable => false,
    }
}

fn pricing_capability_label(
    value: &crate::domains::pricing_catalog::PricingCapability,
) -> &'static str {
    match value {
        crate::domains::pricing_catalog::PricingCapability::Indexing => "indexing",
        crate::domains::pricing_catalog::PricingCapability::Embedding => "embedding",
        crate::domains::pricing_catalog::PricingCapability::Answer => "answer",
        crate::domains::pricing_catalog::PricingCapability::Vision => "vision",
        crate::domains::pricing_catalog::PricingCapability::GraphExtract => "graph_extract",
    }
}

fn pricing_billing_unit_label(
    value: &crate::domains::pricing_catalog::PricingBillingUnit,
) -> &'static str {
    match value {
        crate::domains::pricing_catalog::PricingBillingUnit::Per1MInputTokens => {
            "per_1m_input_tokens"
        }
        crate::domains::pricing_catalog::PricingBillingUnit::Per1MOutputTokens => {
            "per_1m_output_tokens"
        }
        crate::domains::pricing_catalog::PricingBillingUnit::Per1MTokens => "per_1m_tokens",
        crate::domains::pricing_catalog::PricingBillingUnit::FixedPerCall => "fixed_per_call",
    }
}

fn map_attempt_payloads(
    attempt_nos: &[i32],
    jobs: &[IngestionJobRow],
) -> HashMap<i32, IngestionExecutionPayload> {
    let mut payloads = HashMap::new();
    for (attempt_no, job) in attempt_nos.iter().copied().zip(jobs.iter()) {
        if let Ok(payload) = repositories::parse_ingestion_execution_payload(job) {
            payloads.insert(attempt_no, payload);
        }
    }
    payloads
}

fn map_attempt_jobs<'a>(
    attempt_nos: &[i32],
    jobs: &'a [IngestionJobRow],
) -> HashMap<i32, &'a IngestionJobRow> {
    attempt_nos.iter().copied().zip(jobs.iter()).collect()
}

fn group_stage_events_by_attempt(
    stage_events: &[RuntimeIngestionStageEventRow],
) -> BTreeMap<i32, Vec<RuntimeIngestionStageEventRow>> {
    let mut grouped = BTreeMap::new();
    for event in stage_events {
        grouped.entry(event.attempt_no).or_insert_with(Vec::new).push(event.clone());
    }
    grouped
}

fn attempt_status(
    runtime_run: &RuntimeIngestionRunRow,
    job: Option<&IngestionJobRow>,
    stage_events: &[RuntimeIngestionStageEventRow],
) -> String {
    if let Some(job) = job {
        if let Some(status) =
            job.result_json.get("terminal_status").and_then(serde_json::Value::as_str)
        {
            return status.to_string();
        }
        match job.status.as_str() {
            "retryable_failed" => return "failed".to_string(),
            "queued" => return "queued".to_string(),
            "running" => return "processing".to_string(),
            _ => {}
        }
    }
    if stage_events.iter().any(|event| event.status == "failed") {
        "failed".to_string()
    } else if stage_events
        .iter()
        .any(|event| event.stage == "finalizing" && event.status == "completed")
    {
        runtime_run.status.clone()
    } else if stage_events.iter().any(|event| event.status == "started") {
        "processing".to_string()
    } else {
        "queued".to_string()
    }
}

fn attempt_queue_started_at(
    stage_events: &[RuntimeIngestionStageEventRow],
) -> Option<DateTime<Utc>> {
    stage_events.iter().find(|event| event.stage == "accepted").map(|event| event.started_at)
}

fn attempt_started_at(stage_events: &[RuntimeIngestionStageEventRow]) -> Option<DateTime<Utc>> {
    stage_events
        .iter()
        .find(|event| event.stage != "accepted" && event.status == "started")
        .map(|event| event.started_at)
}

fn attempt_finished_at(stage_events: &[RuntimeIngestionStageEventRow]) -> Option<DateTime<Utc>> {
    stage_events
        .iter()
        .rev()
        .find(|event| matches!(event.status.as_str(), "completed" | "failed" | "skipped"))
        .and_then(|event| event.finished_at)
}

fn attempt_partial_history_reason(
    attempt_no: i32,
    runtime_run: &RuntimeIngestionRunRow,
    stage_events: &[RuntimeIngestionStageEventRow],
    payload: Option<&IngestionExecutionPayload>,
) -> Option<String> {
    if payload.is_none() {
        return Some("Attempt metadata predates revision-aware lifecycle snapshots.".to_string());
    }
    if stage_events.is_empty() {
        return Some("Attempt benchmark history is missing stage events.".to_string());
    }
    if stage_events.iter().any(|event| {
        matches!(event.status.as_str(), "completed" | "failed" | "skipped")
            && (event.finished_at.is_none() || event.elapsed_ms.is_none())
    }) {
        return Some("Attempt benchmark history is missing terminal stage timings.".to_string());
    }
    if stage_events.iter().any(|event| {
        matches!(event.stage.as_str(), "embedding_chunks" | "extracting_graph")
            && event.status == "completed"
            && (event.provider_kind.is_none() || event.model_name.is_none())
    }) {
        return Some(
            "Attempt benchmark history is missing provider/model attribution.".to_string(),
        );
    }
    if attempt_no == runtime_run.current_attempt_no
        && matches!(runtime_run.status.as_str(), "ready" | "ready_no_graph" | "failed")
        && (runtime_run.queue_elapsed_ms.is_none()
            || (runtime_run.finished_at.is_some() && runtime_run.total_elapsed_ms.is_none()))
    {
        return Some(
            "Latest attempt predates persisted queue or total elapsed timings.".to_string(),
        );
    }
    None
}

fn decimal_to_f64(value: Option<rust_decimal::Decimal>) -> Option<f64> {
    value.and_then(|value| value.to_f64())
}

fn attempt_activity_status(
    runtime_run: &RuntimeIngestionRunRow,
    attempt_no: i32,
    attempt_status: &str,
    current_activity_status: &str,
) -> &'static str {
    if attempt_no == runtime_run.current_attempt_no {
        return match current_activity_status {
            "queued" => "queued",
            "active" => "active",
            "blocked" => "blocked",
            "retrying" => "retrying",
            "stalled" => "stalled",
            "ready" => "ready",
            "failed" => "failed",
            _ => "active",
        };
    }
    match attempt_status {
        "ready" | "ready_no_graph" => "ready",
        "failed" => "failed",
        "processing" => "active",
        _ => "queued",
    }
}

fn attempt_last_activity_at(
    runtime_run: &RuntimeIngestionRunRow,
    attempt_no: i32,
    stage_events: &[RuntimeIngestionStageEventRow],
) -> Option<DateTime<Utc>> {
    if attempt_no == runtime_run.current_attempt_no {
        return runtime_run.last_activity_at;
    }
    stage_events.iter().rev().find_map(|event| event.finished_at.or(Some(event.started_at)))
}

fn stage_attribution_source(row: &AttemptStageAccountingRow, event_stage: &str) -> &'static str {
    let metadata_source = row
        .pricing_snapshot_json
        .get("stage_ownership")
        .or_else(|| row.token_usage_json.get("stage_ownership"))
        .and_then(|value| value.get("attribution_source"))
        .and_then(serde_json::Value::as_str);
    match metadata_source {
        Some("stage_native") => "stage_native",
        Some("reconciled") => "reconciled",
        _ if row.stage == event_stage => "stage_native",
        _ => "reconciled",
    }
}

fn surface_warnings(rows: &[RuntimeDocumentRow]) -> Vec<ApiWarningBody> {
    let blocked_count = rows.iter().filter(|row| row.activity_status == "blocked").count();
    let stalled_count = rows.iter().filter(|row| row.activity_status == "stalled").count();
    let partial_accounting_count =
        rows.iter().filter(|row| row.accounting_status != "priced").count();
    let mut warnings = Vec::new();
    if blocked_count > 0 {
        warnings.push(blocked_activity_warning(format!(
            "{blocked_count} document(s) are waiting on a blocking condition before progress can continue."
        )));
    }
    if stalled_count > 0 {
        warnings.push(stalled_activity_warning(format!(
            "{stalled_count} document(s) have no recent visible activity."
        )));
    }
    if partial_accounting_count > 0 {
        warnings.push(partial_accounting_warning(format!(
            "{partial_accounting_count} document(s) still expose partial or unpriced stage accounting."
        )));
    }
    warnings
}

fn detail_warnings(
    runtime_run: &RuntimeIngestionRunRow,
    attempts: &[RuntimeAttemptResponse],
) -> Vec<ApiWarningBody> {
    let mut warnings = Vec::new();
    let activity_status = attempts
        .first()
        .map(|attempt| attempt.activity_status.as_str())
        .unwrap_or(runtime_run.activity_status.as_str());
    if activity_status == "blocked" {
        warnings.push(blocked_activity_warning(
            runtime_run.latest_error_message.clone().unwrap_or_else(|| {
                "This document is waiting on an explicit blocking condition.".to_string()
            }),
        ));
    }
    if activity_status == "stalled" {
        warnings.push(stalled_activity_warning(
            runtime_run.latest_error_message.clone().unwrap_or_else(|| {
                "This document has no recent visible processing activity.".to_string()
            }),
        ));
    }
    if attempts.first().is_some_and(|attempt| attempt.cost_summary.accounting_status != "priced") {
        warnings.push(partial_accounting_warning(
            "Latest attempt accounting is partial or still unpriced.",
        ));
    }
    warnings
}

fn attempt_warnings(activity_status: &str, accounting_status: &str) -> Vec<ApiWarningBody> {
    let mut warnings = Vec::new();
    if activity_status == "blocked" {
        warnings.push(blocked_activity_warning(
            "This attempt is blocked and waiting on another condition to clear.",
        ));
    }
    if activity_status == "stalled" {
        warnings
            .push(stalled_activity_warning("This attempt has no recent visible stage activity."));
    }
    if accounting_status != "priced" {
        warnings.push(partial_accounting_warning(
            "This attempt still has partial or unpriced stage accounting.",
        ));
    }
    warnings
}

async fn load_runtime_document_contribution_summary(
    state: &AppState,
    project_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
) -> Result<ResolvedContributionSummary, ApiError> {
    let Some(document_id) = document_id else {
        return Ok(ResolvedContributionSummary {
            chunk_count: None,
            graph_node_count: None,
            graph_edge_count: None,
        });
    };

    let cached = repositories::get_runtime_document_contribution_summary_by_document_id(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .filter(|row| revision_id.is_none() || row.revision_id == revision_id);
    if let Some(row) = cached {
        return Ok(ResolvedContributionSummary {
            chunk_count: row.chunk_count.and_then(|value| usize::try_from(value).ok()),
            graph_node_count: usize::try_from(row.admitted_graph_node_count).ok(),
            graph_edge_count: usize::try_from(row.admitted_graph_edge_count).ok(),
        });
    }

    let chunk_count =
        repositories::count_chunks_by_document(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let graph_counts = match revision_id {
        Some(revision_id) => repositories::count_runtime_graph_contributions_by_document_revision(
            &state.persistence.postgres,
            project_id,
            document_id,
            revision_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?,
        None => repositories::count_runtime_graph_contributions_by_document(
            &state.persistence.postgres,
            project_id,
            document_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?,
    };

    Ok(ResolvedContributionSummary {
        chunk_count: Some(usize::try_from(chunk_count).unwrap_or_default()),
        graph_node_count: Some(usize::try_from(graph_counts.node_count).unwrap_or_default()),
        graph_edge_count: Some(usize::try_from(graph_counts.edge_count).unwrap_or_default()),
    })
}
