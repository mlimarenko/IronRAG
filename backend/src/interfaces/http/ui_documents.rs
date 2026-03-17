use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::{
        HeaderMap, HeaderValue,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::IntoResponse,
};
use futures::{StreamExt, stream};
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ui_documents::{DocumentDetailModel, DocumentListItem, DocumentSurfaceModel},
    infra::{repositories, ui_queries},
    interfaces::http::{
        router_support::{ApiError, map_runtime_lifecycle_error},
        ui_support::{UiSessionContext, load_active_ui_context},
    },
    services::runtime_ingestion::{
        QueueRuntimeUploadRequest, RuntimeUploadFileInput, delete_runtime_run_and_rebuild,
        queue_new_runtime_upload, reprocess_runtime_run_and_rebuild, requeue_runtime_run,
    },
    shared::file_extract,
};

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/ui/documents/surface", axum::routing::get(get_documents_surface))
        .route("/ui/documents/upload", axum::routing::post(upload_documents))
        .route("/ui/documents/{id}/content", axum::routing::get(download_document_content))
        .route(
            "/ui/documents/{id}",
            axum::routing::get(get_document_detail).delete(delete_document_item),
        )
        .route("/ui/documents/{id}/retry", axum::routing::post(retry_document_item))
        .route("/ui/documents/{id}/reprocess", axum::routing::post(reprocess_document_item))
}

#[derive(Debug, Clone, serde::Serialize)]
struct UploadDocumentsResponse {
    accepted_rows: Vec<DocumentListItem>,
}

async fn load_surface_model(
    state: &AppState,
    ui_session: &UiSessionContext,
) -> Result<DocumentSurfaceModel, ApiError> {
    let active = load_active_ui_context(state, ui_session).await?;
    ui_queries::load_documents_surface(
        &state.persistence.postgres,
        &state.bulk_ingest_hardening_services.ingest_activity,
        active.project.id,
        &active.project.name,
        file_extract::UI_ACCEPTED_UPLOAD_FORMATS,
        state.ui_runtime.upload_max_size_mb,
    )
    .await
    .map_err(|_| ApiError::Internal)
}

async fn load_runtime_run_in_active_project(
    state: &AppState,
    ui_session: &UiSessionContext,
    runtime_run_id: Uuid,
) -> Result<
    (crate::interfaces::http::ui_support::UiActiveContext, repositories::RuntimeIngestionRunRow),
    ApiError,
> {
    let active = load_active_ui_context(state, ui_session).await?;
    let run =
        repositories::get_runtime_ingestion_run_by_id(&state.persistence.postgres, runtime_run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("document item {runtime_run_id} not found"))
            })?;
    if run.project_id != active.project.id {
        return Err(ApiError::NotFound(format!("document item {runtime_run_id} not found")));
    }
    Ok((active, run))
}

async fn get_documents_surface(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<DocumentSurfaceModel>, ApiError> {
    Ok(Json(load_surface_model(&state, &ui_session).await?))
}

async fn upload_documents(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadDocumentsResponse>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let upload_batch_id = Uuid::now_v7();
    let upload_limit_bytes = state.ui_runtime.upload_max_size_mb.saturating_mul(1024 * 1024);
    let mut requests = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("invalid multipart payload".into()))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" && field_name != "files" {
            continue;
        }

        let file_name = field
            .file_name()
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));
        let mime_type = field.content_type().map(std::string::ToString::to_string);
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
            project_id: active.project.id,
            upload_batch_id: Some(upload_batch_id),
            requested_by: Some(ui_session.email.clone()),
            trigger_kind: "ui_upload".to_string(),
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
    let mut accepted_rows = Vec::with_capacity(queued_results.len());
    for queued in queued_results {
        let queued = queued.map_err(|error| ApiError::BadRequest(error.to_string()))?;
        info!(
            user_id = %ui_session.user_id,
            workspace_id = %active.workspace.id,
            project_id = %active.project.id,
            runtime_ingestion_run_id = %queued.runtime_run.id,
            file_kind = %queued.runtime_run.file_type,
            "accepted ui documents upload"
        );
        accepted_rows.push(
            ui_queries::load_document_row(
                &state.persistence.postgres,
                &state.bulk_ingest_hardening_services.ingest_activity,
                &queued.runtime_run,
                &active.project.name,
            )
            .await
            .map_err(|_| ApiError::Internal)?,
        );
    }

    Ok(Json(UploadDocumentsResponse { accepted_rows }))
}

async fn get_document_detail(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DocumentDetailModel>, ApiError> {
    let (active, run) = load_runtime_run_in_active_project(&state, &ui_session, id).await?;
    let detail = ui_queries::load_document_detail(
        &state.persistence.postgres,
        &state.bulk_ingest_hardening_services.ingest_activity,
        &run,
        &active.project.name,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(detail))
}

fn download_file_name(file_name: &str) -> String {
    let stem = file_name.rsplit_once('.').map(|(name, _)| name).unwrap_or(file_name).trim();
    let sanitized = stem
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '"' | '\'' | '\n' | '\r' | '\t' => '_',
            other => other,
        })
        .collect::<String>();
    if sanitized.is_empty() { "document".to_string() } else { sanitized }
}

async fn download_document_content(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let (_, run) = load_runtime_run_in_active_project(&state, &ui_session, id).await?;
    let extracted =
        repositories::get_runtime_extracted_content_by_run(&state.persistence.postgres, run.id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("document item {id} has no extracted content"))
            })?;
    let content_text = extracted.content_text.ok_or_else(|| {
        ApiError::NotFound(format!("document item {id} has no extracted text available"))
    })?;
    if content_text.trim().is_empty() {
        return Err(ApiError::NotFound(format!(
            "document item {id} has no extracted text available"
        )));
    }

    let download_name = format!("{}-extracted.txt", download_file_name(&run.file_name));
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    let content_disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download_name))
            .map_err(|_| ApiError::Internal)?;
    headers.insert(CONTENT_DISPOSITION, content_disposition);

    Ok((headers, content_text))
}

async fn delete_document_item(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, run) = load_runtime_run_in_active_project(&state, &ui_session, id).await?;
    let mutation = delete_runtime_run_and_rebuild(&state, &run, Some(&ui_session.email))
        .await
        .map_err(map_runtime_lifecycle_error)?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "mutationId": mutation.map(|row| row.id),
    })))
}

async fn retry_document_item(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DocumentListItem>, ApiError> {
    let (active, run) = load_runtime_run_in_active_project(&state, &ui_session, id).await?;

    if run.status != "failed" {
        return Err(ApiError::BadRequest("document item is not retryable".into()));
    }

    let (retried, _) = requeue_runtime_run(&state, &run, Some(&ui_session.email), "ui_retry", None)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(
        ui_queries::load_document_row(
            &state.persistence.postgres,
            &state.bulk_ingest_hardening_services.ingest_activity,
            &retried,
            &active.project.name,
        )
        .await
        .map_err(|_| ApiError::Internal)?,
    ))
}

async fn reprocess_document_item(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DocumentListItem>, ApiError> {
    let (active, run) = load_runtime_run_in_active_project(&state, &ui_session, id).await?;

    if matches!(run.status.as_str(), "queued" | "processing") {
        return Err(ApiError::BadRequest("document item is still processing".into()));
    }

    let (reprocessed, _) =
        reprocess_runtime_run_and_rebuild(&state, &run, Some(&ui_session.email), "ui_reprocess")
            .await
            .map_err(|_| ApiError::Internal)?;

    Ok(Json(
        ui_queries::load_document_row(
            &state.persistence.postgres,
            &state.bulk_ingest_hardening_services.ingest_activity,
            &reprocessed,
            &active.project.name,
        )
        .await
        .map_err(|_| ApiError::Internal)?,
    ))
}
