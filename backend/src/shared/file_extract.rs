use std::{fmt, path::Path};

use crate::{
    domains::provider_profiles::ProviderModelSelection,
    integrations::llm::LlmGateway,
    shared::extraction::{self, ExtractionOutput},
};
use serde::Serialize;

pub const UI_ACCEPTED_UPLOAD_FORMATS: &[&str] = &["PDF", "DOCX", "TXT", "MD", "Images"];
pub const MULTIPART_UPLOAD_MODE: &str = "multipart_upload_v2";

const TEXT_LIKE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "json", "yaml", "yml", "xml", "html", "htm", "log", "rst",
    "toml", "ini", "cfg", "conf", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "java", "kt",
    "go", "sh", "sql", "css", "scss",
];
const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "heic", "heif"];
const OFFICE_EXTENSIONS: &[&str] = &["docx"];
const TEXT_LIKE_MIME_TYPES: &[&str] = &["application/json", "application/xml", "text/xml"];
const OFFICE_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFileKind {
    TextLike,
    Pdf,
    Image,
    OfficeDocument,
    Binary,
}

impl UploadFileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextLike => "text_like",
            Self::Pdf => "pdf",
            Self::Image => "image",
            Self::OfficeDocument => "office_document",
            Self::Binary => "binary",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::TextLike => "Text",
            Self::Pdf => "PDF",
            Self::Image => "Image",
            Self::OfficeDocument => "DOCX",
            Self::Binary => "Binary",
        }
    }
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
            error_kind: "invalid_multipart_payload",
            message: "invalid multipart payload".to_string(),
            details: UploadRejectionDetails {
                file_name: None,
                detected_format: None,
                mime_type: None,
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause: "The multipart form body could not be parsed.".to_string(),
                operator_action:
                    "Retry the upload using a standard multipart/form-data request body."
                        .to_string(),
            },
        }
    }

    #[must_use]
    pub fn invalid_file_body(file_name: Option<&str>, mime_type: Option<&str>) -> Self {
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
                detected_format,
                mime_type: mime_type.map(str::to_string),
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause: "The upload stream could not be read into a complete file body."
                    .to_string(),
                operator_action:
                    "Retry the upload; if it keeps failing, upload the file individually to isolate the broken part."
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
    pub fn missing_upload_file(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            error_kind: "missing_upload_file",
            message: message.clone(),
            details: UploadRejectionDetails {
                file_name: None,
                detected_format: None,
                mime_type: None,
                file_size_bytes: None,
                upload_limit_mb: None,
                rejection_cause: message,
                operator_action: "Attach a file field named `file` or `files` and retry."
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
                "unsupported file type; only text, pdf, docx, and image uploads are accepted"
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
                "Upload a TXT, MD, PDF, DOCX, or supported image file instead."
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

fn detect_declared_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<UploadFileKind> {
    let normalized_mime =
        mime_type.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase);
    let extension = file_name
        .and_then(|value| Path::new(value).extension().and_then(|ext| ext.to_str()))
        .map(str::to_ascii_lowercase);

    if normalized_mime.as_deref() == Some("application/pdf") || extension.as_deref() == Some("pdf")
    {
        return Some(UploadFileKind::Pdf);
    }
    if normalized_mime.as_deref().is_some_and(|value| value.starts_with("image/"))
        || extension.as_deref().is_some_and(|value| IMAGE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Image);
    }
    if normalized_mime.as_deref().is_some_and(|value| OFFICE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| OFFICE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::OfficeDocument);
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

#[must_use]
pub fn detect_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> UploadFileKind {
    if let Some(file_kind) = detect_declared_upload_file_kind(file_name, mime_type) {
        return file_kind;
    }
    if std::str::from_utf8(file_bytes).is_ok() {
        return UploadFileKind::TextLike;
    }

    UploadFileKind::Binary
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
        UploadFileKind::OfficeDocument => Ok(build_plan_from_extraction(
            file_kind,
            extraction::docx::extract_docx(&file_bytes).map_err(|error| {
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
    vision_provider: &ProviderModelSelection,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    match file_kind {
        UploadFileKind::Image => {
            let detected_mime = mime_type.unwrap_or("image/png");
            let output = extraction::image::extract_image_with_provider(
                gateway,
                vision_provider.provider_kind.as_str(),
                &vision_provider.model_name,
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
    let extracted_text = if content_text.trim().is_empty() { None } else { Some(content_text) };

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

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;
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
                output_text: "ocr text".to_string(),
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
            UploadFileKind::OfficeDocument
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
        assert_eq!(rejection.details().detected_format.as_deref(), Some("Text"));
        assert_eq!(rejection.details().file_size_bytes, Some(2));
    }

    #[test]
    fn creates_structured_limit_rejection() {
        let rejection =
            UploadAdmissionError::file_too_large("manual.pdf", Some("application/pdf"), 1024, 1);

        assert_eq!(rejection.error_kind(), "upload_limit_exceeded");
        assert_eq!(rejection.details().detected_format.as_deref(), Some("PDF"));
        assert_eq!(rejection.details().upload_limit_mb, Some(1));
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

    #[tokio::test]
    async fn runtime_plan_uses_vision_provider_for_images() {
        let provider = ProviderModelSelection {
            provider_kind: crate::domains::provider_profiles::SupportedProviderKind::OpenAi,
            model_name: "gpt-5-mini".to_string(),
        };

        let result = build_runtime_file_extraction_plan(
            &FakeGateway,
            &provider,
            Some("diagram.png"),
            Some("image/png"),
            vec![0x89, 0x50, 0x4E, 0x47],
        )
        .await
        .expect("runtime image extraction");

        assert_eq!(result.file_kind, UploadFileKind::Image);
        assert_eq!(result.extraction_kind, "vision_image");
        assert_eq!(result.provider_kind.as_deref(), Some("openai"));
        assert_eq!(result.model_name.as_deref(), Some("gpt-5-mini"));
        assert_eq!(result.extracted_text.as_deref(), Some("ocr text"));
    }
}
