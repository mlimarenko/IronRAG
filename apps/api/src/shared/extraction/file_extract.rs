use std::str::FromStr;

mod errors;
mod mime_detection;
mod normalization;

use hex;
use sha2::{Digest, Sha256};

use crate::{
    domains::{
        provider_profiles::ProviderModelSelection,
        recognition::{
            LibraryRecognitionPolicy, RecognitionCapability, RecognitionEngine, RecognitionProfile,
            RecognitionStructureTier,
        },
    },
    integrations::{docling, llm::LlmGateway},
    shared::extraction::{
        self, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
    },
};

use self::normalization::{normalize_extracted_content, with_extraction_quality_markers};
pub use self::{
    errors::{
        FileExtractError, UploadAdmissionError, UploadRejectionDetails,
        classify_multipart_file_body_error,
    },
    mime_detection::{detect_upload_file_kind, validate_upload_file_admission},
};

pub const MULTIPART_UPLOAD_MODE: &str = "multipart_upload_v2";
pub const EXTRACTED_CONTENT_PREVIEW_LIMIT: usize = 1_600;
const EXTRACTION_QUALITY_KEY: &str = "content_quality";

const TEXT_LIKE_EXTENSIONS: &[&str] = &[
    // Text and markup
    "txt",
    "md",
    "markdown",
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "xml",
    "svg",
    "log",
    "rst",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    // Web
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "css",
    "scss",
    "less",
    "sass",
    "vue",
    "svelte",
    // Systems
    "rs",
    "go",
    "c",
    "h",
    "cpp",
    "cc",
    "cxx",
    "hpp",
    "hh",
    // JVM
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    // .NET
    "cs",
    "fs",
    "vb",
    "csproj",
    "sln",
    // Scripting
    "py",
    "rb",
    "php",
    "lua",
    "pl",
    "pm",
    "r",
    "jl",
    // Mobile
    "swift",
    "dart",
    "m",
    "mm",
    // Functional
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "cljs",
    "elm",
    // Shell and infra
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "tf",
    "hcl",
    "dockerfile",
    "vagrantfile",
    // Data and query
    "sql",
    "graphql",
    "gql",
    "proto",
    "avsc",
    // Build and config
    "makefile",
    "cmake",
    "ninja",
    "bazel",
    "buck",
];
const HTML_EXTENSIONS: &[&str] = &["html", "htm"];
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];
const DOCX_EXTENSIONS: &[&str] = &["docx"];
const SPREADSHEET_EXTENSIONS: &[&str] = &["csv", "tsv", "xls", "xlsx", "xlsb", "ods"];
const PPTX_EXTENSIONS: &[&str] = &["pptx"];
const HTML_MIME_TYPES: &[&str] = &["text/html", "application/xhtml+xml"];
const TEXT_LIKE_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/jsonl",
    "application/ndjson",
    "application/x-jsonlines",
    "application/x-ndjson",
    "application/xml",
    "text/xml",
    "image/svg+xml",
];
const IMAGE_MIME_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/bmp", "image/webp", "image/tiff"];
const DOCX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"];
const SPREADSHEET_MIME_TYPES: &[&str] = &[
    "text/csv",
    "application/csv",
    "text/tab-separated-values",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.ms-excel.sheet.binary.macroenabled.12",
    "application/vnd.oasis.opendocument.spreadsheet",
];
const PPTX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.presentationml.presentation"];
const GENERIC_BINARY_MIME_TYPES: &[&str] = &["application/octet-stream", "binary/octet-stream"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFileKind {
    TextLike,
    Pdf,
    Image,
    Docx,
    Spreadsheet,
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
            Self::Spreadsheet => "spreadsheet",
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
            Self::Spreadsheet => "Spreadsheet",
            Self::Pptx => "PPTX",
            Self::Binary => "Binary",
        }
    }
}

impl FromStr for UploadFileKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text_like" => Ok(Self::TextLike),
            "pdf" => Ok(Self::Pdf),
            "image" => Ok(Self::Image),
            "docx" => Ok(Self::Docx),
            "spreadsheet" => Ok(Self::Spreadsheet),
            "pptx" => Ok(Self::Pptx),
            "binary" => Ok(Self::Binary),
            _ => Err(()),
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
    pub source_text: Option<String>,
    pub normalized_text: Option<String>,
    pub extraction_error: Option<String>,
    pub extraction_kind: String,
    pub page_count: Option<u32>,
    pub extraction_warnings: Vec<String>,
    pub source_format_metadata: ExtractionSourceMetadata,
    pub structure_hints: ExtractionStructureHints,
    pub source_map: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub usage_json: serde_json::Value,
    pub normalization_profile: String,
    pub extraction_version: Option<String>,
    pub ingest_mode: String,
    /// SHA-256 hex digest of all extracted image bytes (sorted). None when no images present.
    pub image_checksum: Option<String>,
}

pub struct FileExtractionRequest<'a> {
    pub gateway: &'a dyn LlmGateway,
    pub vision_provider: Option<&'a ProviderModelSelection>,
    pub vision_api_key: Option<&'a str>,
    pub vision_base_url: Option<&'a str>,
    pub file_name: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub file_bytes: Vec<u8>,
    pub recognition_policy: &'a LibraryRecognitionPolicy,
}

/// Builds a truncated preview of extracted content for operator-facing surfaces.
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

/// Reads extraction quality markers from a source map and canonical extraction metadata.
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
        .or_else(|| {
            quality
                .and_then(|item| item.get("recognition_engine"))
                .and_then(serde_json::Value::as_str)
                .map(recognition_engine_ocr_source)
        })
        .or_else(|| extraction_kind.starts_with("vision_").then_some("vision_llm".to_string()));
    let warning_count = quality
        .and_then(|item| item.get("warning_count"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(warning_count);

    ExtractedContentQuality { normalization_status, ocr_source, warning_count }
}

/// Builds a local extraction plan for a file payload using only deterministic parsers.
///
/// # Errors
///
/// Returns a [`FileExtractError`] when the payload is binary-only or a parser fails.
pub fn build_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    build_local_file_extraction_plan(file_name, mime_type, &file_bytes)
}

/// Builds a local extraction plan for a file payload using only deterministic parsers.
///
/// # Errors
///
/// Returns a [`FileExtractError`] when the payload is binary-only or a parser fails.
pub fn build_local_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<FileExtractionPlan, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, file_bytes);

    match file_kind {
        UploadFileKind::TextLike => {
            let output = if declared_payload_is_record_jsonl(file_name, mime_type) {
                extraction::record_jsonl::extract_record_jsonl(file_bytes).map_err(|error| {
                    FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
                })?
            } else if mime_detection::declared_payload_is_html(file_name, mime_type)
                || mime_detection::payload_looks_like_html(file_bytes)
            {
                extraction::html_main_content::extract_html_main_content(file_bytes, mime_type)
                    .map_err(|error| FileExtractError::ExtractionFailed {
                        file_kind,
                        message: error.to_string(),
                    })?
            } else {
                extraction::text_like::extract_text_like(file_bytes)
                    .map_err(|_| FileExtractError::InvalidUtf8)?
            };
            Ok(build_plan_from_extraction(
                file_kind,
                output,
                deterministic_recognition_profile(file_kind),
            ))
        }
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Err(runtime_docling_required_error(file_kind))
        }
        UploadFileKind::Image
            if docling_source_format(file_kind, file_name, mime_type).is_some() =>
        {
            Err(runtime_docling_required_error(file_kind))
        }
        UploadFileKind::Image => Err(FileExtractError::ExtractionFailed {
            file_kind,
            message: "image extraction requires a runtime provider context".to_string(),
        }),
        UploadFileKind::Spreadsheet => Ok(build_plan_from_extraction(
            file_kind,
            extraction::tabular::extract_tabular(file_name, mime_type, file_bytes).map_err(
                |error| FileExtractError::ExtractionFailed {
                    file_kind,
                    message: error.to_string(),
                },
            )?,
            deterministic_recognition_profile(file_kind),
        )),
        UploadFileKind::Binary => Err(FileExtractError::UnsupportedBinary),
    }
}

/// Builds a runtime extraction plan, delegating image extraction to the configured provider.
///
/// # Errors
///
/// Returns a [`FileExtractError`] when the payload is binary-only, the image provider is
/// missing, or the underlying parser/provider fails.
pub async fn build_runtime_file_extraction_plan(
    request: FileExtractionRequest<'_>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let FileExtractionRequest {
        gateway,
        vision_provider,
        vision_api_key,
        vision_base_url,
        file_name,
        mime_type,
        file_bytes,
        recognition_policy,
    } = request;
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    if let Some(source_format) = docling_source_format(file_kind, file_name, mime_type) {
        let recognition_profile =
            docling_owned_recognition_profile(file_kind, source_format, recognition_policy)?;
        return match recognition_profile.engine {
            RecognitionEngine::Docling => {
                let output =
                    docling::extract_document(file_name, mime_type, source_format, file_bytes)
                        .await
                        .map_err(|error| FileExtractError::ExtractionFailed {
                            file_kind,
                            message: error.to_string(),
                        })?;
                Ok(build_plan_from_extraction(file_kind, output, recognition_profile))
            }
            RecognitionEngine::Vision => {
                extract_image_with_vision_provider(
                    gateway,
                    vision_provider,
                    vision_api_key,
                    vision_base_url,
                    file_kind,
                    mime_type,
                    &file_bytes,
                    recognition_profile,
                )
                .await
            }
            RecognitionEngine::Native => Err(unsupported_recognition_engine_error(
                file_kind,
                RecognitionEngine::Native,
                RecognitionCapability::ImageOcr,
            )),
        };
    }

    match file_kind {
        UploadFileKind::Image => {
            extract_image_with_vision_provider(
                gateway,
                vision_provider,
                vision_api_key,
                vision_base_url,
                file_kind,
                mime_type,
                &file_bytes,
                RecognitionProfile {
                    capability: RecognitionCapability::ImageOcr,
                    engine: RecognitionEngine::Vision,
                    structure_tier: RecognitionStructureTier::Flat,
                },
            )
            .await
        }
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Err(runtime_docling_required_error(file_kind))
        }
        _ => {
            // Text / HTML / non-Docling spreadsheet parsers are synchronous
            // and CPU-heavy (scraper DOM build, calamine sheet scan). Run
            // them on the blocking pool so the async
            // runtime's worker threads stay available for concurrent jobs'
            // network I/O.
            let file_name_owned = file_name.map(str::to_string);
            let mime_type_owned = mime_type.map(str::to_string);
            let recognition_profile = deterministic_recognition_profile(file_kind);
            tokio::task::spawn_blocking(move || {
                build_local_file_extraction_plan(
                    file_name_owned.as_deref(),
                    mime_type_owned.as_deref(),
                    &file_bytes,
                )
            })
            .await
            .map_err(|join_err| FileExtractError::ExtractionFailed {
                file_kind,
                message: format!("extraction task failed: {join_err}"),
            })?
            .map(|plan| with_plan_recognition_profile(plan, recognition_profile))
        }
    }
}

/// Builds a text-only extraction plan for inline content that is already UTF-8 text.
#[must_use]
pub fn build_inline_text_extraction_plan(text: &str) -> FileExtractionPlan {
    let layout = extraction::build_text_layout_from_content(text);
    let output = ExtractionOutput {
        extraction_kind: "text_like".to_string(),
        content_text: layout.content_text,
        page_count: None,
        warnings: Vec::new(),
        source_metadata: ExtractionSourceMetadata {
            source_format: "text_like".to_string(),
            page_count: None,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({}),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    };
    build_plan_from_extraction(
        UploadFileKind::TextLike,
        output,
        deterministic_recognition_profile(UploadFileKind::TextLike),
    )
}

/// Builds an extraction plan for inline text when the caller knows the logical
/// file name or MIME type. This keeps append/edit materialization on the same
/// deterministic adapter path as uploads.
pub fn build_inline_text_extraction_plan_for_source(
    text: &str,
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Result<FileExtractionPlan, FileExtractError> {
    build_local_file_extraction_plan(file_name, mime_type, text.as_bytes())
}

fn build_plan_from_extraction(
    file_kind: UploadFileKind,
    output: ExtractionOutput,
    recognition_profile: RecognitionProfile,
) -> FileExtractionPlan {
    let ExtractionOutput {
        extraction_kind,
        content_text,
        page_count,
        warnings,
        source_metadata,
        structure_hints,
        source_map,
        provider_kind,
        model_name,
        usage_json,
        extracted_images,
    } = output;
    // Compute image_checksum before dropping the image bytes. Sort by bytes
    // for determinism (order from extractor may vary across runs).
    let image_checksum = if extracted_images.is_empty() {
        None
    } else {
        let mut refs: Vec<&[u8]> =
            extracted_images.iter().map(|i| i.image_bytes.as_slice()).collect();
        refs.sort();
        let mut hasher = Sha256::new();
        for bytes in &refs {
            hasher.update(bytes);
        }
        Some(hex::encode(hasher.finalize()))
    };
    // extracted_images dropped here — image bytes no longer needed.
    drop(extracted_images);

    let normalized = normalize_extracted_content(file_kind, &content_text, &structure_hints);
    let has_source_text = !normalized.source_text.trim().is_empty();
    let has_normalized_text = !normalized.normalized_text.trim().is_empty();
    let source_format_metadata = ExtractionSourceMetadata {
        source_format: source_metadata.source_format,
        page_count: source_metadata.page_count.or(page_count),
        line_count: i32::try_from(normalized.structure_hints.lines.len()).unwrap_or(i32::MAX),
    };
    let source_map = with_extraction_quality_markers(
        source_map,
        &normalized,
        warnings.len(),
        provider_kind.as_deref(),
        recognition_profile,
    );

    FileExtractionPlan {
        file_kind,
        adapter_status: "ready".to_string(),
        source_text: has_source_text.then_some(normalized.source_text),
        normalized_text: has_normalized_text.then_some(normalized.normalized_text),
        extraction_error: None,
        extraction_kind,
        page_count: source_format_metadata.page_count,
        extraction_warnings: warnings,
        source_format_metadata,
        structure_hints: normalized.structure_hints,
        source_map,
        provider_kind,
        model_name,
        usage_json,
        normalization_profile: normalized.normalization_profile,
        extraction_version: Some("runtime_extraction_v1".to_string()),
        ingest_mode: MULTIPART_UPLOAD_MODE.to_string(),
        image_checksum,
    }
}

async fn extract_image_with_vision_provider(
    gateway: &dyn LlmGateway,
    vision_provider: Option<&ProviderModelSelection>,
    api_key: Option<&str>,
    base_url: Option<&str>,
    file_kind: UploadFileKind,
    mime_type: Option<&str>,
    file_bytes: &[u8],
    recognition_profile: RecognitionProfile,
) -> Result<FileExtractionPlan, FileExtractError> {
    let Some(vision_provider) = vision_provider else {
        return Err(FileExtractError::ExtractionFailed {
            file_kind,
            message: "vision binding is not configured for image recognition".to_string(),
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
        file_bytes,
    )
    .await
    .map_err(|error| FileExtractError::ExtractionFailed {
        file_kind,
        message: error.to_string(),
    })?;
    Ok(build_plan_from_extraction(file_kind, output, recognition_profile))
}

fn deterministic_recognition_profile(file_kind: UploadFileKind) -> RecognitionProfile {
    match file_kind {
        UploadFileKind::Spreadsheet => RecognitionProfile {
            capability: RecognitionCapability::TabularParse,
            engine: RecognitionEngine::Native,
            structure_tier: RecognitionStructureTier::Layout,
        },
        _ => RecognitionProfile {
            capability: RecognitionCapability::TextDecode,
            engine: RecognitionEngine::Native,
            structure_tier: RecognitionStructureTier::Paragraph,
        },
    }
}

fn docling_owned_recognition_profile(
    file_kind: UploadFileKind,
    source_format: &str,
    recognition_policy: &LibraryRecognitionPolicy,
) -> Result<RecognitionProfile, FileExtractError> {
    match file_kind {
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Ok(RecognitionProfile {
                capability: RecognitionCapability::DocumentLayout,
                engine: RecognitionEngine::Docling,
                structure_tier: RecognitionStructureTier::Layout,
            })
        }
        UploadFileKind::Image => match recognition_policy.raster_image_engine {
            RecognitionEngine::Docling => Ok(RecognitionProfile {
                capability: RecognitionCapability::ImageOcr,
                engine: RecognitionEngine::Docling,
                structure_tier: RecognitionStructureTier::Layout,
            }),
            RecognitionEngine::Vision => Ok(RecognitionProfile {
                capability: RecognitionCapability::ImageOcr,
                engine: RecognitionEngine::Vision,
                structure_tier: RecognitionStructureTier::Flat,
            }),
            engine => Err(unsupported_recognition_engine_error(
                file_kind,
                engine,
                RecognitionCapability::ImageOcr,
            )),
        },
        UploadFileKind::TextLike | UploadFileKind::Spreadsheet | UploadFileKind::Binary => {
            Err(FileExtractError::ExtractionFailed {
                file_kind,
                message: format!(
                    "docling source format {source_format} is not valid for {file_kind:?}"
                ),
            })
        }
    }
}

fn with_plan_recognition_profile(
    mut plan: FileExtractionPlan,
    recognition_profile: RecognitionProfile,
) -> FileExtractionPlan {
    plan.source_map = with_recognition_source_map(plan.source_map, recognition_profile);
    plan
}

fn with_recognition_source_map(
    source_map: serde_json::Value,
    recognition_profile: RecognitionProfile,
) -> serde_json::Value {
    let mut source_map = match source_map {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    source_map.insert(
        "recognition".to_string(),
        serde_json::json!({
            "engine": recognition_profile.engine.as_str(),
            "capability": recognition_profile.capability.as_str(),
            "structure_tier": recognition_profile.structure_tier.as_str(),
        }),
    );
    serde_json::Value::Object(source_map)
}

fn recognition_engine_ocr_source(engine: &str) -> String {
    match engine {
        "docling" => "docling".to_string(),
        "vision" => "vision_llm".to_string(),
        other => other.to_string(),
    }
}

fn unsupported_recognition_engine_error(
    file_kind: UploadFileKind,
    engine: RecognitionEngine,
    capability: RecognitionCapability,
) -> FileExtractError {
    FileExtractError::ExtractionFailed {
        file_kind,
        message: format!(
            "recognition engine {engine} does not support {} for {} uploads",
            capability.as_str(),
            file_kind.display_name()
        ),
    }
}

fn runtime_docling_required_error(file_kind: UploadFileKind) -> FileExtractError {
    FileExtractError::ExtractionFailed {
        file_kind,
        message: format!(
            "docling runtime extraction is required for {} uploads",
            file_kind.display_name()
        ),
    }
}

fn docling_source_format(
    file_kind: UploadFileKind,
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<&'static str> {
    match file_kind {
        UploadFileKind::Pdf => Some("pdf"),
        UploadFileKind::Docx => Some("docx"),
        UploadFileKind::Pptx => Some("pptx"),
        UploadFileKind::Image => docling_image_source_format(file_name, mime_type),
        UploadFileKind::Spreadsheet => None,
        UploadFileKind::TextLike | UploadFileKind::Binary => None,
    }
}

fn declared_payload_is_record_jsonl(file_name: Option<&str>, mime_type: Option<&str>) -> bool {
    matches!(lower_file_extension(file_name).as_deref(), Some("jsonl" | "ndjson"))
        || matches!(
            mime_type
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some(
                "application/jsonl"
                    | "application/ndjson"
                    | "application/x-jsonlines"
                    | "application/x-ndjson",
            )
        )
}

fn docling_image_source_format(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<&'static str> {
    match lower_file_extension(file_name).as_deref() {
        Some("png") => return Some("png"),
        Some("jpg" | "jpeg") => return Some("jpg"),
        Some("tif" | "tiff") => return Some("tiff"),
        Some("bmp") => return Some("bmp"),
        Some("webp") => return Some("webp"),
        _ => {}
    }

    match mime_type.map(str::to_ascii_lowercase).as_deref() {
        Some("image/png") => Some("png"),
        Some("image/jpeg") => Some("jpg"),
        Some("image/tiff") => Some("tiff"),
        Some("image/bmp") => Some("bmp"),
        Some("image/webp") => Some("webp"),
        _ => None,
    }
}

fn lower_file_extension(file_name: Option<&str>) -> Option<String> {
    file_name
        .and_then(|value| std::path::Path::new(value).extension())
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use anyhow::Result;
    use async_trait::async_trait;
    use image::{DynamicImage, ImageFormat};
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionRequest, VisionResponse,
    };

    fn valid_png_bytes() -> Vec<u8> {
        let image = DynamicImage::new_rgba8(2, 2);
        let mut cursor = Cursor::new(Vec::new());
        if let Err(error) = image.write_to(&mut cursor, ImageFormat::Png) {
            panic!("encode generated png fixture: {error}");
        }
        cursor.into_inner()
    }

    fn valid_xlsx_bytes() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default();

        writer.start_file("[Content_Types].xml", options).expect("content types");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
            )
            .expect("write content types");

        writer.start_file("_rels/.rels", options).expect("root rels");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
            )
            .expect("write root rels");

        writer.start_file("xl/workbook.xml", options).expect("workbook");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
            )
            .expect("write workbook");

        writer.start_file("xl/_rels/workbook.xml.rels", options).expect("workbook rels");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
            )
            .expect("write workbook rels");

        writer.start_file("xl/worksheets/sheet1.xml", options).expect("sheet1");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
      <c r="B1" t="inlineStr"><is><t>Value</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Acme</t></is></c>
      <c r="B2"><v>42</v></c>
    </row>
  </sheetData>
</worksheet>"#,
            )
            .expect("write sheet1");

        writer.finish().expect("finish xlsx");
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
                output_text: "Acme Corp\nBudget 2026".to_string(),
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
            UploadFileKind::Docx
        );
    }

    #[test]
    fn detects_spreadsheet_by_xlsx_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.xlsx"), None, b"binary"),
            UploadFileKind::Spreadsheet
        );
    }

    #[test]
    fn detects_spreadsheet_by_xls_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.xls"), None, b"binary"),
            UploadFileKind::Spreadsheet
        );
    }

    #[test]
    fn detects_tabular_csv_by_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.csv"), None, b"name,value\nacme,42\n"),
            UploadFileKind::Spreadsheet
        );
    }

    #[test]
    fn detects_spreadsheet_by_ods_extension() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.ods"), None, b"binary"),
            UploadFileKind::Spreadsheet
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
    fn treats_svg_as_text_like_xml_not_raster_image() {
        assert_eq!(
            detect_upload_file_kind(
                Some("diagram.svg"),
                Some("image/svg+xml"),
                br#"<svg xmlns="http://www.w3.org/2000/svg"><text>Acme</text></svg>"#,
            ),
            UploadFileKind::TextLike
        );
    }

    #[test]
    fn rejects_declared_heic_as_unsupported_binary() {
        assert_eq!(
            validate_upload_file_admission(Some("photo.heic"), Some("image/heic"), b"binary")
                .unwrap_err(),
            FileExtractError::UnsupportedBinary
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
    fn accepts_spreadsheet_declared_extension_before_utf8_sniffing() {
        assert_eq!(
            detect_upload_file_kind(Some("sheet.xlsx"), None, br"name,value\nacme,42"),
            UploadFileKind::Spreadsheet
        );
    }

    #[test]
    fn accepts_spreadsheet_declared_mime_type_before_utf8_sniffing() {
        assert_eq!(
            detect_upload_file_kind(
                Some("spreadsheet"),
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
                br"name,value\nacme,42",
            ),
            UploadFileKind::Spreadsheet
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
    fn accepts_unknown_extension_text_payload_via_content_sniffing() {
        assert_eq!(
            detect_upload_file_kind(
                Some(".env.backup.20260116-162838"),
                Some("application/octet-stream"),
                b"DATABASE_URL=postgres://localhost:5432/app\nAPI_KEY=redacted\n",
            ),
            UploadFileKind::TextLike
        );
    }

    #[test]
    fn extracts_record_jsonl_by_extension() {
        let plan = build_file_extraction_plan(
            Some("events.jsonl"),
            Some("application/x-ndjson"),
            br#"{"id":"event-1","kind":"event","occurredAt":"2026-04-28T09:00:00Z","text":"created item"}"#.to_vec(),
        )
        .expect("record jsonl extraction");

        assert_eq!(plan.source_format_metadata.source_format, "record_jsonl");
        assert_eq!(plan.extraction_kind, "record_jsonl");
        let source_text = plan.source_text.as_deref().expect("source text");
        assert!(source_text.contains("[source_profile source_format=record_jsonl"));
        assert!(source_text.contains("unit_count=1"));
        assert!(source_text.contains(
            "[unit_id=event-1 unit_kind=event occurred_at=2026-04-28T09:00:00+00:00] created item"
        ));
    }

    #[test]
    fn rejects_invalid_record_jsonl_by_explicit_format() {
        let error = build_file_extraction_plan(
            Some("events.ndjson"),
            Some("application/x-ndjson"),
            b"not-json".to_vec(),
        )
        .unwrap_err();

        assert!(matches!(error, FileExtractError::ExtractionFailed { .. }));
    }

    #[test]
    fn still_rejects_explicit_unsupported_mime_type() {
        assert_eq!(
            detect_upload_file_kind(Some("clip.mp4"), Some("video/mp4"), b"plain text payload\n",),
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
            &FileExtractError::InvalidUtf8,
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
        let large_text = "IronRAG bulk ingest line.\n".repeat(32 * 1024);
        let plan = match build_file_extraction_plan(
            Some("large-notes.txt"),
            Some("text/plain"),
            large_text.clone().into_bytes(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("large text extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::TextLike);
        assert_eq!(plan.extraction_kind, "text_like");
        assert_eq!(plan.normalized_text.as_deref(), Some(large_text.as_str()));
        assert_eq!(plan.source_format_metadata.source_format, "text_like");
    }

    #[test]
    fn routes_html_uploads_through_html_main_content_extractor() {
        let html = r"
            <html>
                <head><title>Ingest page</title></head>
                <body><main><h1>Docs</h1><p>Canonical only.</p></main></body>
            </html>
        ";

        let plan = match build_file_extraction_plan(
            Some("index.html"),
            Some("text/html; charset=utf-8"),
            html.as_bytes().to_vec(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("html extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::TextLike);
        assert_eq!(plan.extraction_kind, "html_main_content");
        assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("# Docs")));
        assert_eq!(plan.source_format_metadata.source_format, "html_main_content");
    }

    #[test]
    fn local_plan_rejects_docling_owned_pdf_upload() {
        let error = match build_file_extraction_plan(
            Some("manual.pdf"),
            Some("application/pdf"),
            b"%PDF-1.7\n".to_vec(),
        ) {
            Ok(plan) => panic!("pdf upload should require runtime docling: {:?}", plan.file_kind),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            FileExtractError::ExtractionFailed { file_kind: UploadFileKind::Pdf, .. }
        ));
        assert!(error.to_string().contains("docling runtime extraction is required"));
    }

    #[test]
    fn builds_tabular_extraction_plan_for_xlsx_upload() {
        let plan = match build_file_extraction_plan(
            Some("inventory.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            valid_xlsx_bytes(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("xlsx extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
        assert_eq!(plan.extraction_kind, "tabular_text");
        assert_eq!(plan.source_format_metadata.source_format, "xlsx");
        assert_eq!(plan.source_map["recognition"]["engine"], serde_json::json!("native"));
        assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Acme | 42 |")));
    }

    #[test]
    fn builds_tabular_extraction_plan_for_csv_upload() {
        let plan = match build_file_extraction_plan(
            Some("people.csv"),
            Some("text/csv"),
            b"Name,Email\nAlice,alice@example.com\n".to_vec(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("csv extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
        assert_eq!(plan.extraction_kind, "tabular_text");
        assert_eq!(plan.source_format_metadata.source_format, "csv");
        assert!(
            plan.normalized_text
                .as_deref()
                .is_some_and(|text| text.contains("| Alice | alice@example.com |"))
        );
    }

    #[test]
    fn builds_tabular_extraction_plan_for_tsv_upload() {
        let plan = match build_file_extraction_plan(
            Some("people.tsv"),
            Some("text/tab-separated-values"),
            b"Name\tEmail\nAlice\talice@example.com\n".to_vec(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("tsv extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
        assert_eq!(plan.extraction_kind, "tabular_text");
        assert_eq!(plan.source_format_metadata.source_format, "tsv");
        assert!(
            plan.normalized_text
                .as_deref()
                .is_some_and(|text| text.contains("| Alice | alice@example.com |"))
        );
    }

    #[tokio::test]
    async fn runtime_plan_uses_native_tabular_parser_for_xlsx_upload() {
        let policy = LibraryRecognitionPolicy::default();
        let result = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: &FakeGateway,
            vision_provider: None,
            vision_api_key: None,
            vision_base_url: None,
            file_name: Some("inventory.xlsx"),
            mime_type: Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            file_bytes: valid_xlsx_bytes(),
            recognition_policy: &policy,
        })
        .await
        .expect("runtime xlsx extraction");

        assert_eq!(result.file_kind, UploadFileKind::Spreadsheet);
        assert_eq!(result.extraction_kind, "tabular_text");
        assert_eq!(result.source_format_metadata.source_format, "xlsx");
        assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("native"));
        assert_eq!(
            result.source_map["recognition"]["capability"],
            serde_json::json!("tabular_parse")
        );
        assert!(result.provider_kind.is_none());
        assert!(
            result.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |"))
        );
    }

    #[test]
    fn rejects_binary_like_utf8_payloads_with_structured_unsupported_type() {
        let extraction_error = match build_file_extraction_plan(
            Some("unsupported.bin"),
            Some("application/octet-stream"),
            b"\0\x01\x02\x03\n".to_vec(),
        ) {
            Ok(plan) => panic!("binary-ish utf8 payload should be rejected: {:?}", plan.file_kind),
            Err(error) => error,
        };
        let rejection = UploadAdmissionError::from_file_extract_error(
            "unsupported.bin",
            Some("application/octet-stream"),
            5,
            &extraction_error,
        );

        assert_eq!(rejection.error_kind(), "unsupported_upload_type");
        assert_eq!(rejection.details().file_name.as_deref(), Some("unsupported.bin"));
        assert_eq!(rejection.details().detected_format.as_deref(), Some("Binary"));
    }

    #[test]
    fn upload_admission_accepts_spreadsheet_before_persistence() {
        let result = validate_upload_file_admission(
            Some("sheet.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            b"binary xlsx payload",
        );

        assert_eq!(
            result.expect("spreadsheet upload should be admitted"),
            UploadFileKind::Spreadsheet
        );
    }

    #[tokio::test]
    async fn runtime_plan_uses_vision_provider_for_non_docling_images() {
        let provider = ProviderModelSelection {
            provider_kind: crate::domains::provider_profiles::SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4-mini".to_string(),
        };

        let policy = LibraryRecognitionPolicy::default();
        let result = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: &FakeGateway,
            vision_provider: Some(&provider),
            vision_api_key: Some("test-key"),
            vision_base_url: None,
            file_name: Some("diagram.gif"),
            mime_type: Some("image/gif"),
            file_bytes: valid_png_bytes(),
            recognition_policy: &policy,
        })
        .await;
        let result = match result {
            Ok(plan) => plan,
            Err(error) => panic!("runtime image extraction: {error}"),
        };

        assert_eq!(result.file_kind, UploadFileKind::Image);
        assert_eq!(result.extraction_kind, "vision_image");
        assert_eq!(result.provider_kind.as_deref(), Some("openai"));
        assert_eq!(result.model_name.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(result.normalized_text.as_deref(), Some("Acme Corp\nBudget 2026"));
        assert_eq!(result.source_format_metadata.source_format, "image");
        let quality = extraction_quality_from_source_map(
            &result.source_map,
            &result.extraction_kind,
            result.extraction_warnings.len(),
        );
        assert_eq!(quality.normalization_status, ExtractionNormalizationStatus::Verbatim);
        assert_eq!(quality.ocr_source.as_deref(), Some("vision_llm"));
        assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("vision"));
        assert_eq!(result.source_map["recognition"]["capability"], serde_json::json!("image_ocr"));
        assert_eq!(result.source_map["recognition"]["structure_tier"], serde_json::json!("flat"));
    }

    #[tokio::test]
    async fn runtime_plan_uses_vision_policy_for_static_raster_images() {
        let provider = ProviderModelSelection {
            provider_kind: crate::domains::provider_profiles::SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4-mini".to_string(),
        };
        let policy = LibraryRecognitionPolicy { raster_image_engine: RecognitionEngine::Vision };

        let result = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: &FakeGateway,
            vision_provider: Some(&provider),
            vision_api_key: Some("test-key"),
            vision_base_url: None,
            file_name: Some("scan.png"),
            mime_type: Some("image/png"),
            file_bytes: valid_png_bytes(),
            recognition_policy: &policy,
        })
        .await
        .expect("vision policy should route static raster image to vision binding");

        assert_eq!(result.extraction_kind, "vision_image");
        assert_eq!(result.provider_kind.as_deref(), Some("openai"));
        assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("vision"));
    }

    #[tokio::test]
    async fn runtime_plan_requires_vision_binding_for_vision_image_policy() {
        let policy = LibraryRecognitionPolicy { raster_image_engine: RecognitionEngine::Vision };

        let error = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: &FakeGateway,
            vision_provider: None,
            vision_api_key: None,
            vision_base_url: None,
            file_name: Some("scan.png"),
            mime_type: Some("image/png"),
            file_bytes: valid_png_bytes(),
            recognition_policy: &policy,
        })
        .await
        .expect_err("vision policy must fail loudly when binding is missing");

        assert!(error.to_string().contains("vision binding is not configured"));
    }

    #[test]
    fn docling_policy_keeps_static_raster_images_on_docling() {
        let profile = docling_owned_recognition_profile(
            UploadFileKind::Image,
            "png",
            &LibraryRecognitionPolicy::default(),
        )
        .expect("docling policy should support static raster image");

        assert_eq!(profile.engine, RecognitionEngine::Docling);
        assert_eq!(profile.capability, RecognitionCapability::ImageOcr);
        assert_eq!(profile.structure_tier, RecognitionStructureTier::Layout);
    }

    #[test]
    fn builds_truncated_content_preview_without_mutating_body() {
        let preview = build_extracted_content_preview(Some("Alpha Beta Gamma"), 5);

        assert_eq!(preview.text.as_deref(), Some("Alpha"));
        assert!(preview.truncated);
    }

    #[test]
    fn resolves_docling_owned_formats() {
        assert_eq!(
            docling_source_format(UploadFileKind::Pdf, Some("manual.pdf"), None),
            Some("pdf")
        );
        assert_eq!(
            docling_source_format(UploadFileKind::Image, Some("scan.webp"), None),
            Some("webp")
        );
    }

    #[test]
    fn leaves_non_docling_formats_on_their_canonical_paths() {
        assert_eq!(
            docling_source_format(UploadFileKind::Spreadsheet, Some("sheet.csv"), None),
            None
        );
        assert_eq!(
            docling_source_format(UploadFileKind::Spreadsheet, Some("sheet.tsv"), None),
            None
        );
        assert_eq!(
            docling_source_format(
                UploadFileKind::Spreadsheet,
                Some("sheet.xlsx"),
                Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
            ),
            None
        );
        assert_eq!(docling_source_format(UploadFileKind::Image, Some("scan.gif"), None), None);
    }
}
