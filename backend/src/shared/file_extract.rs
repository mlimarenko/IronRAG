use std::{fmt, path::Path};

use crate::{
    domains::provider_profiles::ProviderModelSelection,
    integrations::llm::LlmGateway,
    shared::extraction::{self, ExtractionOutput},
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileExtractError {
    UnsupportedBinary,
    InvalidUtf8,
    ExtractionFailed(String),
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
            Self::ExtractionFailed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FileExtractError {}

#[must_use]
pub fn detect_upload_file_kind(
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
    if normalized_mime.as_deref().is_some_and(|value| OFFICE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| OFFICE_EXTENSIONS.contains(&value))
    {
        return UploadFileKind::OfficeDocument;
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
            extraction::pdf::extract_pdf(&file_bytes)
                .map_err(|error| FileExtractError::ExtractionFailed(error.to_string()))?,
        )),
        UploadFileKind::OfficeDocument => Ok(build_plan_from_extraction(
            file_kind,
            extraction::docx::extract_docx(&file_bytes)
                .map_err(|error| FileExtractError::ExtractionFailed(error.to_string()))?,
        )),
        UploadFileKind::Image => Err(FileExtractError::ExtractionFailed(
            "image extraction requires a runtime provider context".to_string(),
        )),
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
            .map_err(|error| FileExtractError::ExtractionFailed(error.to_string()))?;
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
