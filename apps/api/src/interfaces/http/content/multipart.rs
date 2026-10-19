use axum::{
    extract::multipart::{Field, Multipart, MultipartError},
    http::StatusCode,
};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        content::types::{normalize_optional_document_hint, normalize_optional_text},
        router_support::ApiError,
    },
    shared::extraction::file_extract::{
        MultipartFileBodyErrorKind, UploadAdmissionError, multipart_file_body_error,
    },
};

#[derive(Debug)]
pub struct ParsedUploadMultipart {
    pub external_key: Option<String>,
    pub parent_external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub title: Option<String>,
    pub document_hint: Option<String>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct ParsedReplaceMultipart {
    pub idempotency_key: Option<String>,
    pub document_hint: Option<String>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub visibility_intent_id: Option<Uuid>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
}

struct ParsedMultipartFile {
    file_name: String,
    mime_type: Option<String>,
    file_bytes: Vec<u8>,
}

/// Every scalar/part a document upload or revision replacement may carry. Kept
/// as one type so both multipart parsers share a single field-dispatch loop
/// instead of two near-identical ones that drift and clone.
#[derive(Default)]
struct RawMultipartFields {
    external_key: Option<String>,
    parent_external_key: Option<String>,
    idempotency_key: Option<String>,
    title: Option<String>,
    language_code: Option<String>,
    source_uri: Option<String>,
    visibility_intent_id: Option<String>,
    document_hint: Option<String>,
    file: Option<ParsedMultipartFile>,
}

/// Reads every field of a multipart body, rejecting anything outside `allowed`.
/// The caller maps the raw values onto its own parsed shape; this is the single
/// place the field names and allow-list check live.
async fn read_multipart_fields(
    state: &AppState,
    mut multipart: Multipart,
    allowed: fn(&str) -> bool,
    context: &'static str,
) -> Result<RawMultipartFields, ApiError> {
    let mut fields = RawMultipartFields::default();
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        warn!(error = %error, multipart = context, "rejecting invalid multipart payload");
        map_content_multipart_payload_error(state, &error)
    })? {
        let field_name = field.name().unwrap_or_default();
        if !allowed(field_name) {
            return Err(ApiError::BadRequest("unknown multipart field".to_string()));
        }
        match field_name {
            "external_key" => {
                fields.external_key = Some(read_text_field(field, "external_key").await?);
            }
            "parent_external_key" => {
                fields.parent_external_key =
                    Some(read_text_field(field, "parent_external_key").await?);
            }
            "idempotency_key" => {
                fields.idempotency_key = Some(read_text_field(field, "idempotency_key").await?);
            }
            "title" => fields.title = Some(read_text_field(field, "title").await?),
            "language_code" => {
                fields.language_code = Some(read_text_field(field, "language_code").await?)
            }
            "source_uri" => fields.source_uri = Some(read_text_field(field, "source_uri").await?),
            "visibility_intent_id" => {
                fields.visibility_intent_id =
                    Some(read_text_field(field, "visibility_intent_id").await?)
            }
            "document_hint" => {
                fields.document_hint = Some(read_text_field(field, "document_hint").await?);
            }
            "file" => fields.file = Some(read_multipart_file_field(state, field).await?),
            _ => {}
        }
    }
    Ok(fields)
}

pub(super) async fn parse_upload_multipart(
    state: &AppState,
    multipart: Multipart,
) -> Result<ParsedUploadMultipart, ApiError> {
    let fields = read_multipart_fields(state, multipart, is_allowed_upload_field, "upload").await?;
    let file = fields.file.ok_or_else(|| {
        ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file("missing file"))
    })?;
    Ok(ParsedUploadMultipart {
        external_key: fields.external_key.and_then(normalize_optional_text),
        parent_external_key: fields.parent_external_key.and_then(normalize_optional_text),
        idempotency_key: fields.idempotency_key.and_then(normalize_optional_text),
        title: fields.title.and_then(normalize_optional_text),
        document_hint: normalize_optional_document_hint(fields.document_hint)?,
        file_name: file.file_name,
        mime_type: file.mime_type,
        file_bytes: file.file_bytes,
    })
}

pub(super) async fn parse_replace_multipart(
    state: &AppState,
    multipart: Multipart,
) -> Result<ParsedReplaceMultipart, ApiError> {
    let fields =
        read_multipart_fields(state, multipart, is_allowed_replace_field, "replace").await?;
    let file = fields.file.ok_or_else(|| {
        ApiError::from_upload_admission(UploadAdmissionError::missing_upload_file("missing file"))
    })?;
    Ok(ParsedReplaceMultipart {
        idempotency_key: fields.idempotency_key.and_then(normalize_optional_text),
        document_hint: normalize_optional_document_hint(fields.document_hint)?,
        title: fields.title.and_then(normalize_optional_text),
        language_code: fields.language_code.and_then(normalize_optional_text),
        source_uri: fields.source_uri.and_then(normalize_optional_text),
        visibility_intent_id: fields
            .visibility_intent_id
            .map(|raw| {
                Uuid::parse_str(raw.trim())
                    .map_err(|_| ApiError::BadRequest("invalid visibility_intent_id".to_owned()))
            })
            .transpose()?,
        file_name: file.file_name,
        mime_type: file.mime_type,
        file_bytes: file.file_bytes,
    })
}

fn is_allowed_upload_field(field_name: &str) -> bool {
    matches!(
        field_name,
        "external_key"
            | "parent_external_key"
            | "idempotency_key"
            | "title"
            | "document_hint"
            | "file"
    )
}

fn is_allowed_replace_field(field_name: &str) -> bool {
    matches!(
        field_name,
        "idempotency_key"
            | "document_hint"
            | "title"
            | "language_code"
            | "source_uri"
            | "visibility_intent_id"
            | "file"
    )
}

/// Reads one scalar text part, naming it in the rejection.
async fn read_text_field(field: Field<'_>, name: &str) -> Result<String, ApiError> {
    field.text().await.map_err(|_| ApiError::BadRequest(format!("invalid {name}")))
}

/// Filename is semantic identity, not a generated artifact name. Refuse missing
/// or blank names before reading bytes so identical retransmissions cannot mint
/// distinct request fingerprints.
fn multipart_source_filename(raw: Option<&str>) -> Result<String, ApiError> {
    raw.filter(|name| !name.trim().is_empty()).map(ToString::to_string).ok_or_else(|| {
        ApiError::BadRequest("file part requires an explicit nonblank filename".to_owned())
    })
}

/// Reads an explicitly named source file; no semantic field is nonce-generated.
async fn read_multipart_file_field(
    state: &AppState,
    mut field: Field<'_>,
) -> Result<ParsedMultipartFile, ApiError> {
    let file_name = multipart_source_filename(field.file_name())?;
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
    let failure_kind = multipart_file_body_error_kind(error);
    let rejection = match failure_kind {
        MultipartFileBodyErrorKind::InvalidBody => {
            UploadAdmissionError::invalid_multipart_payload()
        }
        MultipartFileBodyErrorKind::SizeLimit | MultipartFileBodyErrorKind::StreamFailure => {
            multipart_file_body_error(
                None,
                None,
                state.ui_runtime.upload_max_size_mb,
                failure_kind,
                &error.body_text(),
            )
        }
    };
    ApiError::from_upload_admission(rejection)
}

fn map_content_multipart_file_body_error(
    state: &AppState,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    error: &MultipartError,
) -> ApiError {
    let failure_kind = multipart_file_body_error_kind(error);
    ApiError::from_upload_admission(multipart_file_body_error(
        file_name,
        mime_type,
        state.ui_runtime.upload_max_size_mb,
        failure_kind,
        &error.body_text(),
    ))
}

fn multipart_file_body_error_kind(error: &MultipartError) -> MultipartFileBodyErrorKind {
    match error.status() {
        StatusCode::PAYLOAD_TOO_LARGE => MultipartFileBodyErrorKind::SizeLimit,
        status if status.is_server_error() => MultipartFileBodyErrorKind::StreamFailure,
        _ => MultipartFileBodyErrorKind::InvalidBody,
    }
}

pub(super) fn resolve_upload_external_key(
    explicit_external_key: Option<String>,
    file_name: &str,
) -> Option<String> {
    explicit_external_key
        .and_then(normalize_optional_text)
        .or_else(|| normalize_optional_text(file_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        is_allowed_replace_field, is_allowed_upload_field, multipart_source_filename,
        resolve_upload_external_key,
    };

    #[test]
    fn source_filename_is_explicit_and_stable_for_retransmission() {
        for raw in [None, Some(""), Some(" \t\n ")] {
            assert!(
                multipart_source_filename(raw).is_err(),
                "missing/blank filename must not become a nonce"
            );
        }
        let explicit = Some(" safe.txt ");
        assert_eq!(multipart_source_filename(explicit).unwrap(), " safe.txt ");
        assert_eq!(
            multipart_source_filename(explicit).unwrap(),
            multipart_source_filename(explicit).unwrap()
        );
    }

    #[tokio::test]
    async fn actual_multipart_part_requires_a_nonblank_filename() {
        use axum::{
            body::Body,
            extract::{FromRequest, Multipart},
            http::Request,
        };
        for filename in [None, Some(""), Some("   "), Some("safe.txt")] {
            let parameter =
                filename.map(|name| format!("; filename=\"{name}\"")).unwrap_or_default();
            let body = format!(
                "--boundary\r\nContent-Disposition: form-data; name=\"file\"{parameter}\r\nContent-Type: text/plain\r\n\r\nsafe\r\n--boundary--\r\n"
            );
            let request = Request::post("/")
                .header("content-type", "multipart/form-data; boundary=boundary")
                .body(Body::from(body))
                .unwrap();
            let mut multipart = Multipart::from_request(request, &()).await.unwrap();
            let part = multipart.next_field().await.unwrap().unwrap();
            let parsed = multipart_source_filename(part.file_name());
            if filename == Some("safe.txt") {
                assert_eq!(parsed.unwrap(), "safe.txt");
            } else {
                assert!(parsed.is_err(), "actual unnamed/blank file part must be rejected");
            }
        }
    }

    #[test]
    fn upload_accepts_only_supported_multipart_fields() {
        for field_name in [
            "external_key",
            "parent_external_key",
            "idempotency_key",
            "title",
            "document_hint",
            "file",
        ] {
            assert!(is_allowed_upload_field(field_name));
        }
        assert!(!is_allowed_upload_field("library_id"));
        assert!(!is_allowed_upload_field("unknown"));
        assert!(!is_allowed_upload_field("visibility_policy"));
        assert!(!is_allowed_upload_field("visibilityPolicy"));
    }

    #[test]
    fn revision_replacement_accepts_only_supported_multipart_fields() {
        assert!(is_allowed_replace_field("idempotency_key"));
        assert!(is_allowed_replace_field("document_hint"));
        assert!(!is_allowed_replace_field("visibility_policy"));
        assert!(!is_allowed_replace_field("visibilityPolicy"));
        assert!(is_allowed_replace_field("visibility_intent_id"));
        assert!(is_allowed_replace_field("language_code"));
        assert!(is_allowed_replace_field("source_uri"));
        assert!(is_allowed_replace_field("file"));
        assert!(!is_allowed_replace_field("library_id"));
        assert!(is_allowed_replace_field("title"));
    }

    #[test]
    fn upload_external_key_prefers_explicit_value() {
        assert_eq!(
            resolve_upload_external_key(Some("folder/spec.md".to_string()), "spec.md"),
            Some("folder/spec.md".to_string())
        );
    }

    #[test]
    fn upload_external_key_falls_back_to_multipart_file_name() {
        assert_eq!(
            resolve_upload_external_key(None, "foo1/path/bar/file.txt"),
            Some("foo1/path/bar/file.txt".to_string())
        );
    }

    #[test]
    fn upload_external_key_rejects_empty_values() {
        assert_eq!(resolve_upload_external_key(Some("   ".to_string()), "   "), None);
    }
}
