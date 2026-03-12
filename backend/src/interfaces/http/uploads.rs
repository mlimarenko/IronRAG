use axum::{
    Json, Router,
    extract::{Multipart, State},
};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        content_support::{TextIngestRequest, ingest_plain_text},
        router_support::ApiError,
    },
};

#[derive(serde::Serialize)]
pub struct UploadIngestResponse {
    pub document_id: Uuid,
    pub external_key: String,
    pub chunk_count: usize,
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
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;
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

    let (document_id, chunk_count) = ingest_plain_text(
        &state,
        TextIngestRequest {
            project_id,
            source_id,
            external_key: &external_key,
            title: title.as_deref().or(Some(&external_key)),
            mime_type: mime_type.as_deref(),
            text: &text,
            ingest_mode: "multipart_text_upload_v1",
            extra_metadata: serde_json::json!({ "file_name": external_key }),
        },
    )
    .await?;

    Ok(Json(UploadIngestResponse { document_id, external_key, chunk_count, mime_type }))
}
