use axum::{
    Json, Router,
    extract::{Multipart, State},
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_WRITE, load_project_and_authorize},
        router_support::ApiError,
    },
    services::runtime_ingestion::{
        QueueRuntimeUploadRequest, RuntimeUploadFileInput, queue_new_runtime_upload,
    },
};

#[derive(serde::Serialize)]
pub struct UploadIngestResponse {
    pub ingestion_job_id: Uuid,
    pub external_key: String,
    pub status: String,
    pub stage: String,
    pub mime_type: Option<String>,
    pub file_kind: String,
    pub adapter_status: String,
    pub ingest_mode: String,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/uploads/ingest", axum::routing::post(upload_and_ingest))
}

async fn upload_and_ingest(
    auth: AuthContext,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadIngestResponse>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let mut project_id: Option<Uuid> = None;
    let mut source_id: Option<Uuid> = None;
    let mut title: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart.next_field().await.map_err(|_| {
        warn!("rejecting upload ingestion request with invalid multipart payload");
        ApiError::BadRequest("invalid multipart payload".into())
    })? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "project_id" => {
                let text = field.text().await.map_err(|_| {
                    warn!("rejecting upload ingestion request with unreadable project_id");
                    ApiError::BadRequest("invalid project_id".into())
                })?;
                project_id = Some(
                    text.parse().map_err(|_| {
                        warn!(project_id = %text, "rejecting upload ingestion request with non-uuid project_id");
                        ApiError::BadRequest("project_id must be uuid".into())
                    })?,
                );
            }
            "source_id" => {
                let text = field.text().await.map_err(|_| {
                    warn!("rejecting upload ingestion request with unreadable source_id");
                    ApiError::BadRequest("invalid source_id".into())
                })?;
                source_id = Some(
                    text.parse().map_err(|_| {
                        warn!(source_id = %text, "rejecting upload ingestion request with non-uuid source_id");
                        ApiError::BadRequest("source_id must be uuid".into())
                    })?,
                );
            }
            "title" => {
                title = Some(field.text().await.map_err(|_| {
                    warn!("rejecting upload ingestion request with unreadable title field");
                    ApiError::BadRequest("invalid title".into())
                })?);
            }
            "file" => {
                file_name = field.file_name().map(std::string::ToString::to_string);
                mime_type = field.content_type().map(std::string::ToString::to_string);
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| {
                            warn!(
                                file_name = ?file_name,
                                mime_type = ?mime_type,
                                "rejecting upload ingestion request with unreadable file body",
                            );
                            ApiError::BadRequest("invalid file body".into())
                        })?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let project_id = project_id.ok_or_else(|| {
        warn!("rejecting upload ingestion request without project_id");
        ApiError::BadRequest("missing project_id".into())
    })?;
    let project =
        load_project_and_authorize(&auth, &state, project_id, POLICY_DOCUMENTS_WRITE).await?;
    let file_bytes = file_bytes.ok_or_else(|| {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %project_id,
            source_id = ?source_id,
            "rejecting upload ingestion request without file payload",
        );
        ApiError::BadRequest("missing file".into())
    })?;
    let external_key = file_name.unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));
    let file_size_bytes = file_bytes.len();
    let queued = queue_new_runtime_upload(
        &state,
        QueueRuntimeUploadRequest {
            project_id,
            upload_batch_id: Some(Uuid::now_v7()),
            requested_by: None,
            trigger_kind: "upload_ingest".to_string(),
            parent_job_id: None,
            idempotency_key: Some(format!("upload-ingest:{}:{}", project_id, external_key)),
            file: RuntimeUploadFileInput {
                source_id,
                file_name: external_key.clone(),
                mime_type: mime_type.clone(),
                file_bytes,
                title,
            },
        },
    )
    .await
    .map_err(|error| {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %project_id,
            source_id = ?source_id,
            external_key = %external_key,
            mime_type = ?mime_type,
            file_size_bytes,
            error = %error,
            "rejecting upload ingestion request for unsupported file kind or extraction failure",
        );
        ApiError::BadRequest(error.to_string())
    })?;
    let payload = repositories::parse_ingestion_execution_payload(&queued.ingestion_job)
        .map_err(|_| ApiError::Internal)?;
    let text_len = payload.text.as_deref().map(str::len).unwrap_or_default();
    info!(
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        source_id = ?source_id,
        external_key = %external_key,
        mime_type = ?mime_type,
        file_kind = payload.file_kind.as_deref().unwrap_or("unknown"),
        file_size_bytes,
        text_len,
        "accepted upload ingestion request",
    );

    info!(
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        source_id = ?source_id,
        ingestion_job_id = %queued.ingestion_job.id,
        status = %queued.ingestion_job.status,
        stage = %queued.ingestion_job.stage,
        external_key = %external_key,
        mime_type = ?mime_type,
        file_kind = payload.file_kind.as_deref().unwrap_or("unknown"),
        file_size_bytes,
        text_len,
        "created ingestion job for upload request",
    );

    Ok(Json(UploadIngestResponse {
        ingestion_job_id: queued.ingestion_job.id,
        external_key,
        status: queued.ingestion_job.status,
        stage: queued.ingestion_job.stage,
        mime_type: payload.mime_type,
        file_kind: payload.file_kind.unwrap_or_else(|| "unknown".to_string()),
        adapter_status: payload.adapter_status.unwrap_or_else(|| "ready".to_string()),
        ingest_mode: payload.ingest_mode,
    }))
}
