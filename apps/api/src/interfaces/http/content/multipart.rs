use axum::extract::multipart::{Field, Multipart, MultipartError};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::router_support::ApiError,
    shared::extraction::file_extract::{UploadAdmissionError, classify_multipart_file_body_error},
};

#[derive(Debug)]
pub(super) struct ParsedUploadMultipart {
    pub library_id: Uuid,
    pub idempotency_key: Option<String>,
    pub title: Option<String>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct ParsedReplaceMultipart {
    pub idempotency_key: Option<String>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

struct ParsedMultipartFile {
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
}

pub(super) async fn parse_upload_multipart(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<ParsedUploadMultipart, ApiError> {
    let mut library_id = None;
    let mut idempotency_key = None;
    let mut title = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut file_bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        warn!(error = %error, "rejecting canonical content upload with invalid multipart payload");
        map_content_multipart_payload_error(state, &error)
    })? {
        match field.name().unwrap_or_default() {
            "library_id" => {
                let raw = field
                    .text()
                    .await
                    .map_err(|_| ApiError::BadRequest("invalid library_id".to_string()))?;
                library_id =
                    Some(raw.parse().map_err(|_| {
                        ApiError::BadRequest("library_id must be uuid".to_string())
                    })?);
            }
            "idempotency_key" => {
                idempotency_key =
                    Some(field.text().await.map_err(|_| {
                        ApiError::BadRequest("invalid idempotency_key".to_string())
                    })?);
            }
            "title" => {
                title = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| ApiError::BadRequest("invalid title".to_string()))?,
                );
            }
            "file" => {
                let parsed_file = read_multipart_file_field(state, field).await?;
                file_name = Some(parsed_file.file_name);
                mime_type = parsed_file.mime_type;
                file_bytes = Some(parsed_file.file_bytes);
            }
            _ => {}
        }
    }

    Ok(ParsedUploadMultipart {
        library_id: library_id
            .ok_or_else(|| ApiError::BadRequest("missing library_id".to_string()))?,
        idempotency_key: idempotency_key.and_then(normalize_optional_text),
        title: title.and_then(normalize_optional_text),
        file_name: file_name.unwrap_or_else(|| format!("upload-{}", Uuid::now_v7())),
        mime_type,
        file_bytes: file_bytes.ok_or_else(|| {
            ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file(
                "missing file",
            ))
        })?,
    })
}

pub(super) async fn parse_replace_multipart(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<ParsedReplaceMultipart, ApiError> {
    let mut idempotency_key = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut file_bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        warn!(error = %error, "rejecting canonical replace mutation with invalid multipart payload");
        map_content_multipart_payload_error(state, &error)
    })? {
        match field.name().unwrap_or_default() {
            "idempotency_key" => {
                idempotency_key = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| ApiError::BadRequest("invalid idempotency_key".to_string()))?,
                );
            }
            "file" => {
                let parsed_file = read_multipart_file_field(state, field).await?;
                file_name = Some(parsed_file.file_name);
                mime_type = parsed_file.mime_type;
                file_bytes = Some(parsed_file.file_bytes);
            }
            _ => {}
        }
    }

    Ok(ParsedReplaceMultipart {
        idempotency_key: idempotency_key.and_then(normalize_optional_text),
        file_name: file_name.unwrap_or_else(|| format!("replace-{}", Uuid::now_v7())),
        mime_type,
        file_bytes: file_bytes.ok_or_else(|| {
            ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file(
                "missing file",
            ))
        })?,
    })
}

async fn read_multipart_file_field(
    state: &AppState,
    mut field: Field<'_>,
) -> Result<ParsedMultipartFile, ApiError> {
    let file_name = field
        .file_name()
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("upload-{}", Uuid::now_v7()));
    let mime_type = field.content_type().map(ToString::to_string);
    let mut file_bytes = Vec::new();

    while let Some(chunk) = field.chunk().await.map_err(|error| {
        map_content_multipart_file_body_error(state, Some(&file_name), mime_type.as_deref(), &error)
    })? {
        file_bytes.extend_from_slice(&chunk);
    }

    Ok(ParsedMultipartFile { file_name, mime_type, file_bytes })
}

fn map_content_multipart_payload_error(state: &AppState, error: &MultipartError) -> ApiError {
    let message = error.to_string();
    let rejection = if message.trim().is_empty() {
        UploadAdmissionError::invalid_multipart_payload()
    } else {
        classify_multipart_file_body_error(
            None,
            None,
            state.ui_runtime.upload_max_size_mb,
            &message,
        )
    };
    ApiError::from_upload_admission(rejection)
}

fn map_content_multipart_file_body_error(
    state: &AppState,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    error: &MultipartError,
) -> ApiError {
    ApiError::from_upload_admission(classify_multipart_file_body_error(
        file_name,
        mime_type,
        state.ui_runtime.upload_max_size_mb,
        &error.to_string(),
    ))
}

fn normalize_optional_text(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
