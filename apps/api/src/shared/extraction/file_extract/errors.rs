use std::fmt;

use serde::Serialize;

use super::{UploadFileKind, mime_detection::detect_declared_upload_file_kind};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRejectionDetails {
    pub file_name: Option<String>,
    pub rejection_kind: Option<String>,
    pub detected_format: Option<String>,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub upload_limit_mb: Option<u64>,
    pub rejection_cause: String,
    pub operator_action: String,
}

#[derive(Debug, Clone)]
pub struct UploadAdmissionError {
    error_kind: &'static str,
    message: String,
    details: UploadRejectionDetails,
}

impl UploadAdmissionError {
    #[must_use]
    pub fn invalid_multipart_payload() -> Self {
        Self {
            error_kind: "multipart_stream_failure",
            message: "multipart upload stream failed".to_string(),
            details: UploadRejectionDetails {
                file_name: None,
                rejection_kind: Some("multipart_stream_failure".to_string()),
                detected_format: None,
                mime_type: None,
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause:
                    "The multipart upload stream could not be parsed into complete fields."
                        .to_string(),
                operator_action:
                    "Retry the upload using a standard multipart/form-data request body."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn invalid_file_body(file_name: Option<&str>, mime_type: Option<&str>) -> Self {
        Self::invalid_file_body_with_cause(
            file_name,
            mime_type,
            "The upload stream could not be read into a complete file body.".to_string(),
        )
    }

    #[must_use]
    pub fn invalid_file_body_with_cause(
        file_name: Option<&str>,
        mime_type: Option<&str>,
        rejection_cause: String,
    ) -> Self {
        let detected_format = detect_declared_upload_file_kind(file_name, mime_type)
            .map(|kind| kind.display_name().to_string());
        let message = file_name.map_or_else(
            || "invalid file body".to_string(),
            |name| format!("invalid file body for {name}"),
        );
        Self {
            error_kind: "invalid_file_body",
            message,
            details: UploadRejectionDetails {
                file_name: file_name.map(str::to_string),
                rejection_kind: Some("invalid_file_body".to_string()),
                detected_format,
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause,
                operator_action:
                    "Retry the upload; if it keeps failing, upload the file individually to isolate the broken part."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn multipart_stream_failure(
        file_name: Option<&str>,
        mime_type: Option<&str>,
        rejection_cause: impl Into<String>,
    ) -> Self {
        let detected_format = detect_declared_upload_file_kind(file_name, mime_type)
            .map(|kind| kind.display_name().to_string());
        let message = file_name.map_or_else(
            || "multipart upload stream failed".to_string(),
            |name| format!("multipart upload stream failed for {name}"),
        );
        Self {
            error_kind: "multipart_stream_failure",
            message,
            details: UploadRejectionDetails {
                file_name: file_name.map(str::to_string),
                rejection_kind: Some("multipart_stream_failure".to_string()),
                detected_format,
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause: rejection_cause.into(),
                operator_action:
                    "Retry the upload; if it keeps failing, re-export the file and upload it individually."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn file_too_large(
        file_name: &str,
        mime_type: Option<&str>,
        file_size_bytes: u64,
        upload_limit_mb: u64,
    ) -> Self {
        let detected_format = detect_declared_upload_file_kind(Some(file_name), mime_type)
            .map(|kind| kind.display_name().to_string());
        Self {
            error_kind: "upload_limit_exceeded",
            message: format!("file {file_name} exceeds the {upload_limit_mb} MB upload limit"),
            details: UploadRejectionDetails {
                file_name: Some(file_name.to_string()),
                rejection_kind: Some("upload_limit_exceeded".to_string()),
                detected_format,
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: Some(file_size_bytes),
                upload_limit_mb: Some(upload_limit_mb),
                rejection_cause: "The file is larger than the configured upload size limit."
                    .to_string(),
                operator_action:
                    "Upload a smaller file, split the document, or raise the configured upload limit."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn upload_batch_too_large(total_size_bytes: u64, upload_limit_mb: u64) -> Self {
        Self {
            error_kind: "upload_limit_exceeded",
            message: format!("upload batch exceeds the {upload_limit_mb} MB upload limit"),
            details: UploadRejectionDetails {
                file_name: None,
                rejection_kind: Some("upload_limit_exceeded".to_string()),
                detected_format: None,
                mime_type: None,
                file_size_bytes: Some(total_size_bytes),
                upload_limit_mb: Some(upload_limit_mb),
                rejection_cause:
                    "The total decoded upload batch is larger than the configured upload size limit."
                        .to_string(),
                operator_action:
                    "Split the batch into smaller uploads, reduce document size, or raise the configured upload limit."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn request_body_too_large(upload_limit_mb: u64) -> Self {
        Self {
            error_kind: "upload_limit_exceeded",
            message: format!("request body exceeded the {upload_limit_mb} MB upload limit"),
            details: UploadRejectionDetails {
                file_name: None,
                rejection_kind: Some("upload_limit_exceeded".to_string()),
                detected_format: None,
                mime_type: None,
                file_size_bytes: None,
                upload_limit_mb: Some(upload_limit_mb),
                rejection_cause:
                    "The MCP request body exceeded the configured upload size limit before it could be fully buffered."
                        .to_string(),
                operator_action:
                    "Split the upload into smaller calls, reduce document size, or raise the configured upload limit."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn streaming_size_limit_exceeded(
        file_name: Option<&str>,
        mime_type: Option<&str>,
        upload_limit_mb: u64,
    ) -> Self {
        let detected_format = detect_declared_upload_file_kind(file_name, mime_type)
            .map(|kind| kind.display_name().to_string());
        let message = file_name.map_or_else(
            || format!("upload exceeded the {upload_limit_mb} MB size limit"),
            |name| format!("file {name} exceeded the {upload_limit_mb} MB upload limit"),
        );
        Self {
            error_kind: "upload_limit_exceeded",
            message,
            details: UploadRejectionDetails {
                file_name: file_name.map(str::to_string),
                rejection_kind: Some("upload_limit_exceeded".to_string()),
                detected_format,
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: None,
                upload_limit_mb: Some(upload_limit_mb),
                rejection_cause:
                    "The upload stream exceeded the configured upload size limit before the file body was fully read."
                        .to_string(),
                operator_action:
                    "Upload a smaller file, split the document, or raise the configured upload limit."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn missing_upload_file(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            error_kind: "missing_upload_file",
            message: message.clone(),
            details: UploadRejectionDetails {
                file_name: None,
                rejection_kind: Some("missing_upload_file".to_string()),
                detected_format: None,
                mime_type: None,
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause: message,
                operator_action: "Attach a file field named `file` and retry.".to_string(),
            },
        }
    }

    #[must_use]
    pub fn from_file_extract_error(
        file_name: &str,
        mime_type: Option<&str>,
        file_size_bytes: u64,
        error: &FileExtractError,
    ) -> Self {
        let error_kind = error.error_kind();
        let message = error.to_string();
        Self {
            error_kind,
            details: UploadRejectionDetails {
                file_name: Some(file_name.to_string()),
                rejection_kind: Some(error_kind.to_string()),
                detected_format: Some(error.detected_kind().display_name().to_string()),
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: Some(file_size_bytes),
                upload_limit_mb: None,
                rejection_cause: error.rejection_cause(),
                operator_action: error.operator_action().to_string(),
            },
            message,
        }
    }

    #[must_use]
    pub const fn error_kind(&self) -> &'static str {
        self.error_kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> &UploadRejectionDetails {
        &self.details
    }
}

impl fmt::Display for UploadAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UploadAdmissionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileExtractError {
    UnsupportedBinary,
    InvalidUtf8,
    ExtractionFailed { file_kind: UploadFileKind, message: String },
}

impl fmt::Display for FileExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBinary => write!(
                f,
                "unsupported file type; only text, pdf, docx, tabular (csv, tsv, xls, xlsx, xlsb, ods), pptx, and image uploads are accepted"
            ),
            Self::InvalidUtf8 => {
                write!(f, "selected file is treated as text-like but could not be decoded as utf-8")
            }
            Self::ExtractionFailed { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FileExtractError {}

impl FileExtractError {
    #[must_use]
    pub const fn detected_kind(&self) -> UploadFileKind {
        match self {
            Self::UnsupportedBinary => UploadFileKind::Binary,
            Self::InvalidUtf8 => UploadFileKind::TextLike,
            Self::ExtractionFailed { file_kind, .. } => *file_kind,
        }
    }

    #[must_use]
    pub const fn error_kind(&self) -> &'static str {
        match self {
            Self::UnsupportedBinary => "unsupported_upload_type",
            Self::InvalidUtf8 => "invalid_text_encoding",
            Self::ExtractionFailed { .. } => "upload_extraction_failed",
        }
    }

    #[must_use]
    pub fn rejection_cause(&self) -> String {
        match self {
            Self::UnsupportedBinary => {
                "The file type is not supported for upload ingestion.".to_string()
            }
            Self::InvalidUtf8 => {
                "The file was detected as text-like but could not be decoded as UTF-8.".to_string()
            }
            Self::ExtractionFailed { message, .. } => message.clone(),
        }
    }

    #[must_use]
    pub const fn operator_action(&self) -> &'static str {
        match self {
            Self::UnsupportedBinary => {
                "Upload a TXT, MD, PDF, DOCX, XLS, XLSX, XLSB, ODS, PPTX, or supported image file instead."
            }
            Self::InvalidUtf8 => {
                "Re-save the file as UTF-8 text or upload a format with a dedicated parser."
            }
            Self::ExtractionFailed { .. } => {
                "Retry the upload; if it keeps failing, inspect the file parser path for this format."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MultipartFileReadFailure {
    StreamFailure,
    InvalidBody,
    SizeLimit,
}

fn classify_multipart_file_read_failure(message: &str) -> MultipartFileReadFailure {
    let normalized = message.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return MultipartFileReadFailure::InvalidBody;
    }
    if [
        "size limit",
        "field exceeded",
        "stream size exceeded",
        "field size exceeded",
        "body too large",
        "larger than the limit",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        return MultipartFileReadFailure::SizeLimit;
    }
    if [
        "multipart",
        "stream",
        "boundary",
        "connection",
        "incomplete field data",
        "failed to read field data",
        "failed to read stream",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        return MultipartFileReadFailure::StreamFailure;
    }
    MultipartFileReadFailure::InvalidBody
}

fn normalize_upload_rejection_cause(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "The upload stream could not be decoded into a complete file body.".to_string()
    } else {
        trimmed.to_string()
    }
}

#[must_use]
pub fn classify_multipart_file_body_error(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    upload_limit_mb: u64,
    error_message: &str,
) -> UploadAdmissionError {
    match classify_multipart_file_read_failure(error_message) {
        MultipartFileReadFailure::SizeLimit => UploadAdmissionError::streaming_size_limit_exceeded(
            file_name,
            mime_type,
            upload_limit_mb,
        ),
        MultipartFileReadFailure::StreamFailure => UploadAdmissionError::multipart_stream_failure(
            file_name,
            mime_type,
            normalize_upload_rejection_cause(error_message),
        ),
        MultipartFileReadFailure::InvalidBody => {
            UploadAdmissionError::invalid_file_body_with_cause(
                file_name,
                mime_type,
                normalize_upload_rejection_cause(error_message),
            )
        }
    }
}
