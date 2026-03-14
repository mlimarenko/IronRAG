use std::path::Path;

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

const MULTIPART_TEXT_UPLOAD_MODE: &str = "multipart_text_upload_v1";
const TEXT_LIKE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "json", "yaml", "yml", "xml", "html", "htm", "log", "rst",
    "toml", "ini", "cfg", "conf", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "java", "kt",
    "go", "sh", "sql", "css", "scss",
];
const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "heic", "heif"];
const TEXT_LIKE_MIME_TYPES: &[&str] = &["application/json", "application/xml", "text/xml"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadFileKind {
    TextLike,
    Pdf,
    Image,
    Binary,
}

impl UploadFileKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TextLike => "text_like",
            Self::Pdf => "pdf",
            Self::Image => "image",
            Self::Binary => "binary",
        }
    }
}

fn detect_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> UploadFileKind {
    let normalized_mime =
        mime_type.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase);
    let extension = file_name
        .and_then(|value| Path::new(value).extension().and_then(|ext| ext.to_str()))
        .map(str::to_ascii_lowercase);

    if normalized_mime.as_deref() == Some("application/pdf") || extension.as_deref() == Some("pdf")
    {
        return UploadFileKind::Pdf;
    }
    if normalized_mime.as_deref().is_some_and(|value| value.starts_with("image/"))
        || extension.as_deref().is_some_and(|value| IMAGE_EXTENSIONS.contains(&value))
    {
        return UploadFileKind::Image;
    }
    if normalized_mime
        .as_deref()
        .is_some_and(|value| value.starts_with("text/") || TEXT_LIKE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| TEXT_LIKE_EXTENSIONS.contains(&value))
        || std::str::from_utf8(file_bytes).is_ok()
    {
        return UploadFileKind::TextLike;
    }

    UploadFileKind::Binary
}

fn decode_upload_text(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<(UploadFileKind, String), ApiError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    match file_kind {
        UploadFileKind::TextLike => {
            String::from_utf8(file_bytes).map(|text| (file_kind, text)).map_err(|_| {
                ApiError::BadRequest(
                    "selected file is treated as text-like but could not be decoded as utf-8"
                        .into(),
                )
            })
        }
        UploadFileKind::Pdf => Err(ApiError::BadRequest(
            "pdf uploads are planned but blocked until backend pdf text extraction is implemented"
                .into(),
        )),
        UploadFileKind::Image => Err(ApiError::BadRequest(
            "image uploads are planned but blocked until backend OCR/extraction is implemented"
                .into(),
        )),
        UploadFileKind::Binary => Err(ApiError::BadRequest(
            "only utf-8 text-like uploads are supported right now; pdf/image adapters are planned"
                .into(),
        )),
    }
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

    let (file_kind, text) =
        decode_upload_text(Some(external_key.as_str()), mime_type.as_deref(), file_bytes).map_err(
            |error| {
                warn!(
                    workspace_id = %project.workspace_id,
                    project_id = %project_id,
                    source_id = ?source_id,
                    external_key = %external_key,
                    mime_type = ?mime_type,
                    file_size_bytes,
                    error = %error,
                    "rejecting upload ingestion request for unsupported file kind or encoding",
                );
                error
            },
        )?;
    if text.trim().is_empty() {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %project_id,
            source_id = ?source_id,
            external_key = %external_key,
            mime_type = ?mime_type,
            file_kind = file_kind.as_str(),
            file_size_bytes,
            "rejecting upload ingestion request with empty file content",
        );
        return Err(ApiError::BadRequest("uploaded file is empty".into()));
    }

    let text_len = text.len();
    let idempotency_key = format!("upload-ingest:{}:{}", project_id, external_key);
    info!(
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        source_id = ?source_id,
        external_key = %external_key,
        mime_type = ?mime_type,
        file_kind = file_kind.as_str(),
        file_size_bytes,
        text_len,
        "accepted upload ingestion request",
    );
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
            "external_key": external_key.clone(),
            "title": title.as_deref().or(Some(&external_key)),
            "mime_type": mime_type.clone(),
            "text": text,
            "file_kind": file_kind.as_str(),
            "adapter_status": "supported_now",
            "ingest_mode": MULTIPART_TEXT_UPLOAD_MODE,
            "extra_metadata": {
                "file_name": external_key.clone(),
                "original_file_name": external_key.clone(),
                "file_kind": file_kind.as_str(),
                "adapter_status": "supported_now",
            },
        }),
    )
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("idx_ingestion_job_idempotency_key") =>
        {
            warn!(
                workspace_id = %project.workspace_id,
                project_id = %project_id,
                source_id = ?source_id,
                external_key = %external_key,
                "duplicate upload ingestion request",
            );
            ApiError::Conflict("an ingestion job already exists for this idempotency key".into())
        }
        _ => ApiError::Internal,
    })?;

    info!(
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        source_id = ?source_id,
        ingestion_job_id = %job.id,
        status = %job.status,
        stage = %job.stage,
        external_key = %external_key,
        mime_type = ?mime_type,
        file_kind = file_kind.as_str(),
        file_size_bytes,
        text_len,
        "created ingestion job for upload request",
    );

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
        file_kind: job
            .payload_json
            .get("file_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(file_kind.as_str())
            .to_string(),
        adapter_status: job
            .payload_json
            .get("adapter_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("supported_now")
            .to_string(),
        ingest_mode: job
            .payload_json
            .get("ingest_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(MULTIPART_TEXT_UPLOAD_MODE)
            .to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pdf_by_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("manual.pdf"), None, b"%PDF-1.7"),
            UploadFileKind::Pdf
        );
    }

    #[test]
    fn detects_image_by_mime_type() {
        assert_eq!(
            detect_upload_file_kind(Some("photo.bin"), Some("image/png"), &[0x89, 0x50, 0x4e]),
            UploadFileKind::Image
        );
    }

    #[test]
    fn accepts_extensionless_utf8_text() {
        assert_eq!(
            detect_upload_file_kind(Some("Dockerfile"), None, b"FROM rust:1.86"),
            UploadFileKind::TextLike
        );
    }

    #[test]
    fn rejects_invalid_utf8_when_file_is_text_like() {
        let result = decode_upload_text(Some("notes.txt"), Some("text/plain"), vec![0xff, 0xfe]);

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
        if let Err(ApiError::BadRequest(message)) = result {
            assert!(message.contains("utf-8"));
        }
    }
}
