use axum::{
    Json, Router,
    extract::{Multipart, State},
};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_WRITE, load_project_and_authorize},
        router_support::ApiError,
    },
};

#[derive(serde::Serialize)]
pub struct UploadIngestResponse {
    pub ingestion_job_id: Uuid,
    pub external_key: String,
    pub status: String,
    pub stage: String,
    pub mime_type: Option<String>,
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

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("invalid multipart payload".into()))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "project_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|_| ApiError::BadRequest("invalid project_id".into()))?;
                project_id = Some(
                    text.parse()
                        .map_err(|_| ApiError::BadRequest("project_id must be uuid".into()))?,
                );
            }
            "source_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|_| ApiError::BadRequest("invalid source_id".into()))?;
                source_id = Some(
                    text.parse()
                        .map_err(|_| ApiError::BadRequest("source_id must be uuid".into()))?,
                );
            }
            "title" => {
                title = Some(
                    field.text().await.map_err(|_| ApiError::BadRequest("invalid title".into()))?,
                );
            }
            "file" => {
                file_name = field.file_name().map(std::string::ToString::to_string);
                mime_type = field.content_type().map(std::string::ToString::to_string);
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| ApiError::BadRequest("invalid file body".into()))?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let project_id = project_id.ok_or_else(|| ApiError::BadRequest("missing project_id".into()))?;
    load_project_and_authorize(&auth, &state, project_id, POLICY_DOCUMENTS_WRITE).await?;
    let file_bytes = file_bytes.ok_or_else(|| ApiError::BadRequest("missing file".into()))?;
    let external_key = file_name.unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));

    let text = String::from_utf8(file_bytes).map_err(|_| {
        ApiError::BadRequest(
            "only utf-8 text-like uploads are supported in foundation stage".into(),
        )
    })?;
    if text.trim().is_empty() {
        return Err(ApiError::BadRequest("uploaded file is empty".into()));
    }

    let idempotency_key = format!("upload-ingest:{}:{}", project_id, external_key);
    let job = repositories::create_ingestion_job(
        &state.persistence.postgres,
        project_id,
        source_id,
        "upload_ingest",
        None,
        None,
        Some(&idempotency_key),
        serde_json::json!({
            "project_id": project_id,
            "source_id": source_id,
            "external_key": external_key,
            "title": title.as_deref().or(Some(&external_key)),
            "mime_type": mime_type.clone(),
            "text": text,
            "ingest_mode": "multipart_text_upload_v1",
            "extra_metadata": { "file_name": external_key },
        }),
    )
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("idx_ingestion_job_idempotency_key") =>
        {
            ApiError::Conflict("an ingestion job already exists for this idempotency key".into())
        }
        _ => ApiError::Internal,
    })?;

    Ok(Json(UploadIngestResponse {
        ingestion_job_id: job.id,
        external_key,
        status: job.status,
        stage: job.stage,
        mime_type: job
            .payload_json
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .map(std::string::ToString::to_string),
    }))
}
