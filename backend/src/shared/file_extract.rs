use std::{fmt, path::Path};

use crate::{
    domains::provider_profiles::ProviderModelSelection,
    integrations::llm::LlmGateway,
    shared::extraction::{self, ExtractionOutput},
};
use serde::Serialize;

pub const MULTIPART_UPLOAD_MODE: &str = "multipart_upload_v2";
pub const EXTRACTED_CONTENT_PREVIEW_LIMIT: usize = 1_600;
const EXTRACTION_QUALITY_KEY: &str = "content_quality";

const TEXT_LIKE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "json", "yaml", "yml", "xml", "html", "htm", "log", "rst",
    "toml", "ini", "cfg", "conf", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "java", "kt",
    "go", "sh", "sql", "css", "scss",
];
const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "heic", "heif"];
const DOCX_EXTENSIONS: &[&str] = &["docx"];
const PPTX_EXTENSIONS: &[&str] = &["pptx"];
const TEXT_LIKE_MIME_TYPES: &[&str] = &["application/json", "application/xml", "text/xml"];
const DOCX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"];
const PPTX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.presentationml.presentation"];
const GENERIC_BINARY_MIME_TYPES: &[&str] = &["application/octet-stream", "binary/octet-stream"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFileKind {
    TextLike,
    Pdf,
    Image,
    Docx,
    Pptx,
    Binary,
}

impl UploadFileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextLike => "text_like",
            Self::Pdf => "pdf",
            Self::Image => "image",
            Self::Docx => "docx",
            Self::Pptx => "pptx",
            Self::Binary => "binary",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::TextLike => "Text",
            Self::Pdf => "PDF",
            Self::Image => "Image",
            Self::Docx => "DOCX",
            Self::Pptx => "PPTX",
            Self::Binary => "Binary",
        }
    }

    #[must_use]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "text_like" => Some(Self::TextLike),
            "pdf" => Some(Self::Pdf),
            "image" => Some(Self::Image),
            "docx" => Some(Self::Docx),
            "pptx" => Some(Self::Pptx),
            "binary" => Some(Self::Binary),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionNormalizationStatus {
    Verbatim,
    Normalized,
}

impl ExtractionNormalizationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verbatim => "verbatim",
            Self::Normalized => "normalized",
        }
    }

    #[must_use]
    pub fn from_source_map(value: Option<&str>) -> Self {
        match value {
            Some("normalized") => Self::Normalized,
            _ => Self::Verbatim,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedContentQuality {
    pub normalization_status: ExtractionNormalizationStatus,
    pub ocr_source: Option<String>,
    pub warning_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedContentPreview {
    pub text: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FileExtractionPlan {
    pub file_kind: UploadFileKind,
    pub adapter_status: String,
    pub extracted_text: Option<String>,
    pub extraction_error: Option<String>,
    pub extraction_kind: String,
    pub page_count: Option<u32>,
    pub extraction_warnings: Vec<String>,
    pub source_map: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub extraction_version: Option<String>,
    pub ingest_mode: String,
}

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
        let message = file_name
            .map(|name| format!("invalid file body for {name}"))
            .unwrap_or_else(|| "invalid file body".to_string());
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
        let message = file_name
            .map(|name| format!("multipart upload stream failed for {name}"))
            .unwrap_or_else(|| "multipart upload stream failed".to_string());
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
            message: format!(
                "upload batch exceeds the {upload_limit_mb} MB upload limit"
            ),
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
        let message = file_name
            .map(|name| format!("file {name} exceeded the {upload_limit_mb} MB upload limit"))
            .unwrap_or_else(|| format!("upload exceeded the {upload_limit_mb} MB size limit"));
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
                operator_action: "Attach a file field named `file` and retry."
                    .to_string(),
            },
        }
    }

    #[must_use]
    pub fn from_file_extract_error(
        file_name: &str,
        mime_type: Option<&str>,
        file_size_bytes: u64,
        error: FileExtractError,
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
                "unsupported file type; only text, pdf, docx, pptx, and image uploads are accepted"
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
                "Upload a TXT, MD, PDF, DOCX, PPTX, or supported image file instead."
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

fn detect_declared_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<UploadFileKind> {
    let normalized_mime = normalized_upload_mime_type(mime_type);
    let extension = normalized_upload_extension(file_name);

    if normalized_mime.as_deref() == Some("application/pdf") || extension.as_deref() == Some("pdf")
    {
        return Some(UploadFileKind::Pdf);
    }
    if normalized_mime.as_deref().is_some_and(|value| value.starts_with("image/"))
        || extension.as_deref().is_some_and(|value| IMAGE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Image);
    }
    if normalized_mime.as_deref().is_some_and(|value| DOCX_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| DOCX_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Docx);
    }
    if normalized_mime.as_deref().is_some_and(|value| PPTX_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| PPTX_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Pptx);
    }
    if normalized_mime
        .as_deref()
        .is_some_and(|value| value.starts_with("text/") || TEXT_LIKE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| TEXT_LIKE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::TextLike);
    }

    None
}

fn normalized_upload_extension(file_name: Option<&str>) -> Option<String> {
    file_name
        .and_then(|value| Path::new(value).extension().and_then(|ext| ext.to_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn normalized_upload_mime_type(mime_type: Option<&str>) -> Option<String> {
    mime_type.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase)
}

fn is_supported_upload_extension(extension: &str) -> bool {
    extension == "pdf"
        || TEXT_LIKE_EXTENSIONS.contains(&extension)
        || IMAGE_EXTENSIONS.contains(&extension)
        || DOCX_EXTENSIONS.contains(&extension)
        || PPTX_EXTENSIONS.contains(&extension)
}

fn is_supported_upload_mime_type(mime_type: &str) -> bool {
    mime_type == "application/pdf"
        || mime_type.starts_with("image/")
        || TEXT_LIKE_MIME_TYPES.contains(&mime_type)
        || mime_type.starts_with("text/")
        || DOCX_MIME_TYPES.contains(&mime_type)
        || PPTX_MIME_TYPES.contains(&mime_type)
}

fn mime_type_is_generic_binary(mime_type: &str) -> bool {
    GENERIC_BINARY_MIME_TYPES.contains(&mime_type)
}

fn declares_unsupported_upload_format(file_name: Option<&str>, mime_type: Option<&str>) -> bool {
    if let Some(extension) = normalized_upload_extension(file_name) {
        if !is_supported_upload_extension(&extension) {
            return true;
        }
    }

    if let Some(mime_type) = normalized_upload_mime_type(mime_type) {
        if !mime_type_is_generic_binary(&mime_type) && !is_supported_upload_mime_type(&mime_type) {
            return true;
        }
    }

    false
}

#[must_use]
pub fn detect_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> UploadFileKind {
    if let Some(file_kind) = detect_declared_upload_file_kind(file_name, mime_type) {
        return file_kind;
    }
    if declares_unsupported_upload_format(file_name, mime_type) {
        return UploadFileKind::Binary;
    }
    if let Ok(decoded_text) = std::str::from_utf8(file_bytes) {
        if !utf8_payload_looks_binary(decoded_text) {
            return UploadFileKind::TextLike;
        }
    }

    UploadFileKind::Binary
}

pub fn validate_upload_file_admission(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<UploadFileKind, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, file_bytes);
    match file_kind {
        UploadFileKind::Binary => Err(FileExtractError::UnsupportedBinary),
        UploadFileKind::TextLike => {
            std::str::from_utf8(file_bytes).map_err(|_| FileExtractError::InvalidUtf8)?;
            Ok(file_kind)
        }
        UploadFileKind::Pdf | UploadFileKind::Image | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Ok(file_kind)
        }
    }
}

fn utf8_payload_looks_binary(decoded_text: &str) -> bool {
    if decoded_text.chars().any(|ch| ch == '\0') {
        return true;
    }

    let non_whitespace_control_count = decoded_text
        .chars()
        .filter(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | '\u{000C}'))
        .count();
    let total_char_count = decoded_text.chars().count();
    if total_char_count == 0 {
        return false;
    }

    non_whitespace_control_count.saturating_mul(5) >= total_char_count
}

#[must_use]
pub fn build_extracted_content_preview(
    content_text: Option<&str>,
    limit: usize,
) -> ExtractedContentPreview {
    let Some(content_text) = content_text.map(str::trim).filter(|value| !value.is_empty()) else {
        return ExtractedContentPreview { text: None, truncated: false };
    };
    let char_count = content_text.chars().count();
    if char_count <= limit {
        return ExtractedContentPreview { text: Some(content_text.to_string()), truncated: false };
    }

    let preview = content_text.chars().take(limit).collect::<String>();
    ExtractedContentPreview { text: Some(preview.trim_end().to_string()), truncated: true }
}

#[must_use]
pub fn extraction_quality_from_source_map(
    source_map: &serde_json::Value,
    extraction_kind: &str,
    warning_count: usize,
) -> ExtractedContentQuality {
    let quality = source_map.get(EXTRACTION_QUALITY_KEY);
    let normalization_status = ExtractionNormalizationStatus::from_source_map(
        quality
            .and_then(|item| item.get("normalization_status"))
            .and_then(serde_json::Value::as_str),
    );
    let ocr_source = quality
        .and_then(|item| item.get("ocr_source"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| extraction_kind.starts_with("vision_").then_some("vision_llm".to_string()));
    let warning_count = quality
        .and_then(|item| item.get("warning_count"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(warning_count);

    ExtractedContentQuality { normalization_status, ocr_source, warning_count }
}

pub fn build_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    build_local_file_extraction_plan(file_name, mime_type, file_bytes)
}

pub fn build_local_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    match file_kind {
        UploadFileKind::TextLike => Ok(build_plan_from_extraction(
            file_kind,
            extraction::text_like::extract_text_like(&file_bytes)
                .map_err(|_| FileExtractError::InvalidUtf8)?,
        )),
        UploadFileKind::Pdf => Ok(build_plan_from_extraction(
            file_kind,
            extraction::pdf::extract_pdf(&file_bytes).map_err(|error| {
                FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
            })?,
        )),
        UploadFileKind::Docx => Ok(build_plan_from_extraction(
            file_kind,
            extraction::docx::extract_docx(&file_bytes).map_err(|error| {
                FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
            })?,
        )),
        UploadFileKind::Pptx => Ok(build_plan_from_extraction(
            file_kind,
            extraction::pptx::extract_pptx(&file_bytes).map_err(|error| {
                FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
            })?,
        )),
        UploadFileKind::Image => Err(FileExtractError::ExtractionFailed {
            file_kind,
            message: "image extraction requires a runtime provider context".to_string(),
        }),
        UploadFileKind::Binary => Err(FileExtractError::UnsupportedBinary),
    }
}

pub async fn build_runtime_file_extraction_plan(
    gateway: &dyn LlmGateway,
    vision_provider: Option<&ProviderModelSelection>,
    api_key: Option<&str>,
    base_url: Option<&str>,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    match file_kind {
        UploadFileKind::Image => {
            let Some(vision_provider) = vision_provider else {
                return Err(FileExtractError::ExtractionFailed {
                    file_kind,
                    message: "vision binding is not configured for image extraction".to_string(),
                });
            };
            let detected_mime = mime_type.unwrap_or("image/png");
            let output = extraction::image::extract_image_with_provider(
                gateway,
                vision_provider.provider_kind.as_str(),
                &vision_provider.model_name,
                api_key.unwrap_or_default(),
                base_url,
                detected_mime,
                &file_bytes,
            )
            .await
            .map_err(|error| FileExtractError::ExtractionFailed {
                file_kind,
                message: error.to_string(),
            })?;
            Ok(build_plan_from_extraction(file_kind, output))
        }
        _ => build_local_file_extraction_plan(file_name, mime_type, file_bytes),
    }
}

fn build_plan_from_extraction(
    file_kind: UploadFileKind,
    output: ExtractionOutput,
) -> FileExtractionPlan {
    let ExtractionOutput {
        extraction_kind,
        content_text,
        page_count,
        warnings,
        source_map,
        provider_kind,
        model_name,
    } = output;
    let normalized = normalize_extracted_content(file_kind, &content_text);
    let has_content = !normalized.content_text.trim().is_empty();
    let source_map = with_extraction_quality_markers(
        source_map,
        &normalized,
        warnings.len(),
        provider_kind.as_deref(),
    );
    let extracted_text = has_content.then_some(normalized.content_text);

    FileExtractionPlan {
        file_kind,
        adapter_status: "ready".to_string(),
        extracted_text,
        extraction_error: None,
        extraction_kind,
        page_count,
        extraction_warnings: warnings,
        source_map,
        provider_kind,
        model_name,
        extraction_version: Some("runtime_extraction_v1".to_string()),
        ingest_mode: MULTIPART_UPLOAD_MODE.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedExtractedContent {
    content_text: String,
    normalization_status: ExtractionNormalizationStatus,
    ocr_source: Option<String>,
}

fn normalize_extracted_content(
    file_kind: UploadFileKind,
    content_text: &str,
) -> NormalizedExtractedContent {
    match file_kind {
        UploadFileKind::Image => {
            let normalized_text = normalize_image_ocr_text(content_text);
            let normalization_status = if normalized_text.trim() == content_text.trim() {
                ExtractionNormalizationStatus::Verbatim
            } else {
                ExtractionNormalizationStatus::Normalized
            };
            NormalizedExtractedContent {
                content_text: normalized_text,
                normalization_status,
                ocr_source: Some("vision_llm".to_string()),
            }
        }
        _ => NormalizedExtractedContent {
            content_text: content_text.to_string(),
            normalization_status: ExtractionNormalizationStatus::Verbatim,
            ocr_source: None,
        },
    }
}

fn normalize_image_ocr_text(content_text: &str) -> String {
    let normalized_newlines = content_text.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized_newlines.lines().map(str::trim).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let mut start = 0usize;
    while start < lines.len() {
        let line = lines[start];
        if line.is_empty() {
            start += 1;
            continue;
        }
        if is_ocr_wrapper_line(line) {
            start += 1;
            continue;
        }
        break;
    }

    let cleaned = lines[start..]
        .iter()
        .map(|line| strip_wrapper_label_prefix(line))
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = cleaned.trim().trim_matches('`').trim().to_string();
    if cleaned.is_empty() { content_text.trim().to_string() } else { cleaned }
}

fn is_ocr_wrapper_line(line: &str) -> bool {
    let normalized = line.trim().trim_matches(':').to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "transcription"
            | "ocr"
            | "ocr text"
            | "recognized text"
            | "recognized text from the image"
            | "extracted text"
            | "extracted text from the image"
            | "text from the image"
            | "visible text"
    ) || (normalized.contains("image")
        && (normalized.contains("extracted")
            || normalized.contains("transcription")
            || normalized.contains("recognized")
            || normalized.contains("visible text")
            || normalized.contains("readable text")
            || normalized.contains("ocr")))
}

fn strip_wrapper_label_prefix(line: &str) -> String {
    let trimmed = line.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    for prefix in [
        "transcription:",
        "ocr:",
        "ocr text:",
        "recognized text:",
        "recognized text from the image:",
        "extracted text:",
        "extracted text from the image:",
        "text from the image:",
        "visible text:",
    ] {
        if lowercase.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn with_extraction_quality_markers(
    source_map: serde_json::Value,
    normalized: &NormalizedExtractedContent,
    warning_count: usize,
    provider_kind: Option<&str>,
) -> serde_json::Value {
    let mut source_map = match source_map {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    source_map.insert(
        EXTRACTION_QUALITY_KEY.to_string(),
        serde_json::json!({
            "normalization_status": normalized.normalization_status.as_str(),
            "ocr_source": normalized
                .ocr_source
                .as_deref()
                .or(provider_kind.map(|_| "vision_llm")),
            "warning_count": warning_count,
        }),
    );
    serde_json::Value::Object(source_map)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use anyhow::Result;
    use async_trait::async_trait;
    use image::{DynamicImage, ImageFormat};
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionRequest, VisionResponse,
    };

    fn valid_png_bytes() -> Vec<u8> {
        let image = DynamicImage::new_rgba8(2, 2);
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).expect("encode generated png fixture");
        cursor.into_inner()
    }

    struct FakeGateway;

    #[async_trait]
    impl LlmGateway for FakeGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            unreachable!("generate is not used in file extraction tests")
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in file extraction tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in file extraction tests")
        }

        async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
            Ok(VisionResponse {
                provider_kind: request.provider_kind,
                model_name: request.model_name,
                output_text:
                    "Below is the extracted text from the image.\n\nTranscription:\nAcme Corp\nBudget 2026"
                        .to_string(),
                usage_json: serde_json::json!({}),
            })
        }
    }

    fn build_minimal_pdf_bytes() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let single_page_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Integer(14)]),
                Operation::new("Td", vec![Object::Integer(72), Object::Integer(720)]),
                Operation::new("Tj", vec![Object::string_literal("Quarterly graph report")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document
            .add_object(Stream::new(dictionary! {}, content.encode().expect("encode pdf stream")));
        document.objects.insert(
            single_page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![single_page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("save pdf");
        bytes
    }

    #[test]
    fn detects_pdf_by_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("manual.pdf"), None, b"%PDF-1.7"),
            UploadFileKind::Pdf
        );
    }

    #[test]
    fn detects_docx_by_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("notes.docx"), None, b"binary"),
            UploadFileKind::Docx
        );
    }

    #[test]
    fn detects_pptx_by_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("deck.pptx"), None, b"binary"),
            UploadFileKind::Pptx
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
    fn rejects_utf8_payloads_with_unsupported_declared_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.xlsx"), None, br#"name,value\nacme,42"#),
            UploadFileKind::Binary
        );
    }

    #[test]
    fn rejects_utf8_payloads_with_unsupported_declared_mime_type() {
        assert_eq!(
            detect_upload_file_kind(
                Some("spreadsheet"),
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
                br#"name,value\nacme,42"#,
            ),
            UploadFileKind::Binary
        );
    }

    #[test]
    fn rejects_extensionless_utf8_payloads_with_nul_bytes_as_binary() {
        assert_eq!(
            detect_upload_file_kind(Some("payload.bin"), None, b"\0\x01\x02\x03\n"),
            UploadFileKind::Binary
        );
    }

    #[test]
    fn rejects_invalid_utf8_when_file_is_text_like() {
        let result =
            build_file_extraction_plan(Some("notes.txt"), Some("text/plain"), vec![0xff, 0xfe]);

        assert!(matches!(result, Err(FileExtractError::InvalidUtf8)));
    }

    #[test]
    fn converts_invalid_utf8_into_structured_upload_rejection() {
        let rejection = UploadAdmissionError::from_file_extract_error(
            "notes.txt",
            Some("text/plain"),
            2,
            FileExtractError::InvalidUtf8,
        );

        assert_eq!(rejection.error_kind(), "invalid_text_encoding");
        assert_eq!(rejection.details().file_name.as_deref(), Some("notes.txt"));
        assert_eq!(rejection.details().rejection_kind.as_deref(), Some("invalid_text_encoding"));
        assert_eq!(rejection.details().detected_format.as_deref(), Some("Text"));
        assert_eq!(rejection.details().file_size_bytes, Some(2));
    }

    #[test]
    fn creates_structured_limit_rejection() {
        let rejection =
            UploadAdmissionError::file_too_large("manual.pdf", Some("application/pdf"), 1024, 1);

        assert_eq!(rejection.error_kind(), "upload_limit_exceeded");
        assert_eq!(rejection.details().rejection_kind.as_deref(), Some("upload_limit_exceeded"));
        assert_eq!(rejection.details().detected_format.as_deref(), Some("PDF"));
        assert_eq!(rejection.details().upload_limit_mb, Some(1));
    }

    #[test]
    fn classifies_stream_limit_body_errors_as_upload_limit_exceeded() {
        let rejection = classify_multipart_file_body_error(
            Some("large.pdf"),
            Some("application/pdf"),
            4,
            "field size exceeded",
        );

        assert_eq!(rejection.error_kind(), "upload_limit_exceeded");
        assert_eq!(rejection.details().rejection_kind.as_deref(), Some("upload_limit_exceeded"));
        assert_eq!(rejection.details().upload_limit_mb, Some(4));
    }

    #[test]
    fn classifies_stream_failures_as_multipart_stream_failure() {
        let rejection = classify_multipart_file_body_error(
            Some("report.pdf"),
            Some("application/pdf"),
            4,
            "failed to read stream to end",
        );

        assert_eq!(rejection.error_kind(), "multipart_stream_failure");
        assert_eq!(rejection.details().rejection_kind.as_deref(), Some("multipart_stream_failure"));
    }

    #[test]
    fn accepts_large_utf8_text_upload_plan() {
        let large_text = "RustRAG bulk ingest line.\n".repeat(32 * 1024);
        let plan = build_file_extraction_plan(
            Some("large-notes.txt"),
            Some("text/plain"),
            large_text.clone().into_bytes(),
        )
        .expect("large text extraction plan");

        assert_eq!(plan.file_kind, UploadFileKind::TextLike);
        assert_eq!(plan.extraction_kind, "text_like");
        assert_eq!(plan.extracted_text.as_deref(), Some(large_text.as_str()));
    }

    #[test]
    fn builds_pdf_extraction_plan_for_minimal_pdf_upload() {
        let plan = build_file_extraction_plan(
            Some("manual.pdf"),
            Some("application/pdf"),
            build_minimal_pdf_bytes(),
        )
        .expect("pdf extraction plan");

        assert_eq!(plan.file_kind, UploadFileKind::Pdf);
        assert_eq!(plan.extraction_kind, "pdf_text");
        assert_eq!(plan.page_count, Some(1));
        assert!(
            plan.extracted_text
                .as_deref()
                .is_some_and(|text| text.contains("Quarterly graph report"))
        );
    }

    #[test]
    fn rejects_binary_like_utf8_payloads_with_structured_unsupported_type() {
        let rejection = UploadAdmissionError::from_file_extract_error(
            "unsupported.bin",
            Some("application/octet-stream"),
            5,
            build_file_extraction_plan(
                Some("unsupported.bin"),
                Some("application/octet-stream"),
                b"\0\x01\x02\x03\n".to_vec(),
            )
            .expect_err("binary-ish utf8 payload should be rejected"),
        );

        assert_eq!(rejection.error_kind(), "unsupported_upload_type");
        assert_eq!(rejection.details().file_name.as_deref(), Some("unsupported.bin"));
        assert_eq!(rejection.details().detected_format.as_deref(), Some("Binary"));
    }

    #[test]
    fn upload_admission_rejects_unsupported_declared_extension_before_persistence() {
        let result = validate_upload_file_admission(
            Some("sheet.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            br#"name,value\nacme,42"#,
        );

        assert!(matches!(result, Err(FileExtractError::UnsupportedBinary)));
    }

    #[tokio::test]
    async fn runtime_plan_uses_vision_provider_for_images() {
        let provider = ProviderModelSelection {
            provider_kind: crate::domains::provider_profiles::SupportedProviderKind::OpenAi,
            model_name: "gpt-5-mini".to_string(),
        };

        let result = build_runtime_file_extraction_plan(
            &FakeGateway,
            Some(&provider),
            Some("test-key"),
            None,
            Some("diagram.png"),
            Some("image/png"),
            valid_png_bytes(),
        )
        .await
        .expect("runtime image extraction");

        assert_eq!(result.file_kind, UploadFileKind::Image);
        assert_eq!(result.extraction_kind, "vision_image");
        assert_eq!(result.provider_kind.as_deref(), Some("openai"));
        assert_eq!(result.model_name.as_deref(), Some("gpt-5-mini"));
        assert_eq!(result.extracted_text.as_deref(), Some("Acme Corp\nBudget 2026"));
        let quality = extraction_quality_from_source_map(
            &result.source_map,
            &result.extraction_kind,
            result.extraction_warnings.len(),
        );
        assert_eq!(quality.normalization_status, ExtractionNormalizationStatus::Normalized);
        assert_eq!(quality.ocr_source.as_deref(), Some("vision_llm"));
    }

    #[test]
    fn builds_truncated_content_preview_without_mutating_body() {
        let preview = build_extracted_content_preview(Some("Alpha Beta Gamma"), 5);

        assert_eq!(preview.text.as_deref(), Some("Alpha"));
        assert!(preview.truncated);
    }
}
