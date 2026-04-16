use std::str::FromStr;

mod errors;
mod mime_detection;
mod normalization;

use crate::{
    domains::provider_profiles::ProviderModelSelection,
    integrations::llm::{LlmGateway, VisionRequest},
    shared::extraction::{
        self, ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
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
    "yaml",
    "yml",
    "xml",
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
const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "heic", "heif"];
const DOCX_EXTENSIONS: &[&str] = &["docx"];
const SPREADSHEET_EXTENSIONS: &[&str] = &["csv", "tsv", "xls", "xlsx", "xlsb", "ods"];
const PPTX_EXTENSIONS: &[&str] = &["pptx"];
const HTML_MIME_TYPES: &[&str] = &["text/html", "application/xhtml+xml"];
const TEXT_LIKE_MIME_TYPES: &[&str] = &["application/json", "application/xml", "text/xml"];
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
            let output = if mime_detection::declared_payload_is_html(file_name, mime_type)
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
            Ok(build_plan_from_extraction(file_kind, output))
        }
        UploadFileKind::Pdf => Ok(build_plan_from_extraction(
            file_kind,
            extraction::pdf::extract_pdf(file_bytes).map_err(|error| {
                FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
            })?,
        )),
        UploadFileKind::Docx => Ok(build_plan_from_extraction(
            file_kind,
            extraction::docx::extract_docx(file_bytes).map_err(|error| {
                FileExtractError::ExtractionFailed { file_kind, message: error.to_string() }
            })?,
        )),
        UploadFileKind::Spreadsheet => Ok(build_plan_from_extraction(
            file_kind,
            extraction::tabular::extract_tabular(file_name, mime_type, file_bytes).map_err(
                |error| FileExtractError::ExtractionFailed {
                    file_kind,
                    message: error.to_string(),
                },
            )?,
        )),
        UploadFileKind::Pptx => Ok(build_plan_from_extraction(
            file_kind,
            extraction::pptx::extract_pptx(file_bytes).map_err(|error| {
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

/// Builds a runtime extraction plan, delegating image extraction to the configured provider.
///
/// # Errors
///
/// Returns a [`FileExtractError`] when the payload is binary-only, the image provider is
/// missing, or the underlying parser/provider fails.
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
        UploadFileKind::Pdf | UploadFileKind::Docx => {
            // Run the synchronous parser on the tokio blocking pool so the
            // async runtime's worker threads stay free for I/O during what
            // is otherwise a pure-CPU stall. `extract_pdf` walks the lopdf
            // object graph and `extract_docx` runs zip + xml decoding —
            // both can easily hold a worker thread for hundreds of ms on a
            // large document, which previously blocked concurrent ingest
            // futures from making progress on their own LLM calls.
            let mut output =
                tokio::task::spawn_blocking(
                    move || -> Result<ExtractionOutput, FileExtractError> {
                        match file_kind {
                            UploadFileKind::Pdf => extraction::pdf::extract_pdf(&file_bytes)
                                .map_err(|error| FileExtractError::ExtractionFailed {
                                    file_kind,
                                    message: error.to_string(),
                                }),
                            UploadFileKind::Docx => extraction::docx::extract_docx(&file_bytes)
                                .map_err(|error| FileExtractError::ExtractionFailed {
                                    file_kind,
                                    message: error.to_string(),
                                }),
                            _ => unreachable!(),
                        }
                    },
                )
                .await
                .map_err(|join_err| FileExtractError::ExtractionFailed {
                    file_kind,
                    message: format!("extraction task failed: {join_err}"),
                })??;

            if let Some(vision_provider) = vision_provider {
                if !output.extracted_images.is_empty() {
                    // Move ownership of `extracted_images` into the vision
                    // describer so it can drop each image's bytes the
                    // moment they have been base64-encoded into a request.
                    // The previous `&output.extracted_images` borrow forced
                    // a `.clone()` per image (147 MB held in flight per
                    // 294-image PDF, ×12 docs in parallel = ~1.8 GB just on
                    // the in-flight clones), and on top of that the entire
                    // `Vec<ExtractedImage>` was kept alive on `output` for
                    // the rest of the document lifecycle (chunking, embed,
                    // graph extract, finalize), inflating the worker's
                    // resident set well beyond the 2 GB cgroup limit and
                    // causing repeated OOM kills. After the describer
                    // returns, the vec is empty and the bytes are gone.
                    let images = std::mem::take(&mut output.extracted_images);
                    let result = describe_extracted_images(
                        gateway,
                        vision_provider.provider_kind.as_str(),
                        &vision_provider.model_name,
                        api_key.unwrap_or_default(),
                        base_url,
                        images,
                    )
                    .await;
                    append_image_descriptions_to_output(&mut output, &result.descriptions);
                    output.provider_kind = result.provider_kind;
                    output.model_name = result.model_name;
                    output.usage_json = result.usage_json;
                }
            }

            Ok(build_plan_from_extraction(file_kind, output))
        }
        _ => {
            // Text / HTML / spreadsheet / pptx parsers are all synchronous
            // and CPU-heavy (scraper DOM build, calamine sheet scan, zip +
            // xml decode). Run them on the blocking pool so the async
            // runtime's worker threads stay available for concurrent jobs'
            // network I/O.
            let file_name_owned = file_name.map(str::to_string);
            let mime_type_owned = mime_type.map(str::to_string);
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
    build_plan_from_extraction(UploadFileKind::TextLike, output)
}

/// Description of an image extracted from a document, produced by Vision LLM.
#[derive(Debug, Clone)]
pub struct ImageDescriptionBlock {
    pub page: usize,
    pub description: String,
}

const IMAGE_DESCRIPTION_PROMPT: &str = "Describe this image in detail, including any text, data, tables, diagrams, charts, or formulas visible.";
const MIN_IMAGE_DIMENSION: u32 = 50;

/// Result of describing extracted images with vision LLM, including aggregated usage.
pub struct ImageDescriptionResult {
    pub descriptions: Vec<ImageDescriptionBlock>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub usage_json: serde_json::Value,
}

/// Sends extracted images to a Vision LLM for description. Takes the image
/// vector by value and consumes it with `into_iter()` so that each image's
/// `Vec<u8>` is dropped the moment its vision call returns, instead of being
/// held in memory for the entire document lifecycle. Images smaller than
/// 50x50 pixels are skipped (icons/bullets) without ever touching the LLM.
pub async fn describe_extracted_images(
    gateway: &dyn LlmGateway,
    provider_kind: &str,
    model_name: &str,
    api_key: &str,
    base_url: Option<&str>,
    images: Vec<ExtractedImage>,
) -> ImageDescriptionResult {
    let total_count = images.len();
    let eligible: Vec<ExtractedImage> = images
        .into_iter()
        .filter(|img| img.width >= MIN_IMAGE_DIMENSION && img.height >= MIN_IMAGE_DIMENSION)
        .collect();
    let eligible_count = eligible.len();

    if eligible.is_empty() {
        return ImageDescriptionResult {
            descriptions: Vec::new(),
            provider_kind: None,
            model_name: None,
            usage_json: serde_json::json!({}),
        };
    }

    tracing::info!(
        total = total_count,
        eligible = eligible.len(),
        "describing extracted images with vision llm"
    );

    let mut results = Vec::new();
    let mut prompt_tokens_sum: i64 = 0;
    let mut completion_tokens_sum: i64 = 0;
    let mut total_tokens_sum: i64 = 0;
    // `into_iter()` lets us *move* each `ExtractedImage` out of the vector
    // one at a time. The vector's backing buffer is held until the loop
    // finishes, but the individual `image_bytes: Vec<u8>` allocations are
    // dropped at the end of each iteration body — so resident image-byte
    // memory shrinks linearly through the loop instead of staying at peak
    // for the entire document.
    for (idx, image) in eligible.into_iter().enumerate() {
        let page = image.page;
        let request = VisionRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: IMAGE_DESCRIPTION_PROMPT.to_string(),
            image_bytes: image.image_bytes,
            mime_type: image.mime_type,
            api_key_override: Some(api_key.to_string()),
            base_url_override: base_url.map(str::to_string),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        };

        match gateway.vision_extract(request).await {
            Ok(response) => {
                if let Some(v) = response.usage_json.get("prompt_tokens").and_then(|v| v.as_i64()) {
                    prompt_tokens_sum += v;
                }
                if let Some(v) =
                    response.usage_json.get("completion_tokens").and_then(|v| v.as_i64())
                {
                    completion_tokens_sum += v;
                }
                if let Some(v) = response.usage_json.get("total_tokens").and_then(|v| v.as_i64()) {
                    total_tokens_sum += v;
                }
                if !response.output_text.trim().is_empty() {
                    results.push(ImageDescriptionBlock { page, description: response.output_text });
                }
            }
            Err(error) => {
                tracing::warn!(stage = "vision", image_index = idx, error = %error, "Vision LLM description failed, skipping image");
            }
        }
    }

    let described_count = results.len();
    let failed_count = eligible_count - described_count;
    tracing::info!(
        stage = "vision",
        images_described = described_count,
        images_failed = failed_count,
        "Vision LLM descriptions complete"
    );

    ImageDescriptionResult {
        descriptions: results,
        provider_kind: Some(provider_kind.to_string()),
        model_name: Some(model_name.to_string()),
        usage_json: serde_json::json!({
            "prompt_tokens": prompt_tokens_sum,
            "completion_tokens": completion_tokens_sum,
            "total_tokens": total_tokens_sum,
        }),
    }
}

/// Appends image description blocks to the extraction content text.
fn append_image_descriptions_to_output(
    output: &mut ExtractionOutput,
    descriptions: &[ImageDescriptionBlock],
) {
    if descriptions.is_empty() {
        return;
    }

    let mut extra_text = String::new();
    for desc in descriptions {
        extra_text.push_str("\n\n---\n\n");
        extra_text.push_str(&format!("[Image from page {}]\n", desc.page));
        extra_text.push_str(&desc.description);
    }

    output.content_text.push_str(&extra_text);

    // Rebuild structure hints to include the new content
    let updated_layout = extraction::build_text_layout_from_content(&output.content_text);
    output.structure_hints = updated_layout.structure_hints;
    output.source_metadata.line_count =
        i32::try_from(output.structure_hints.lines.len()).unwrap_or(i32::MAX);
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
        source_metadata,
        structure_hints,
        source_map,
        provider_kind,
        model_name,
        usage_json,
        extracted_images: _,
    } = output;
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
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use anyhow::Result;
    use async_trait::async_trait;
    use image::{DynamicImage, ImageFormat};
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

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
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            match content.encode() {
                Ok(bytes) => bytes,
                Err(error) => panic!("encode pdf stream: {error}"),
            },
        ));
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
        if let Err(error) = document.save_to(&mut bytes) {
            panic!("save pdf: {error}");
        }
        bytes
    }

    fn build_minimal_xlsx_bytes() -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        write_xlsx_fixture(
            &mut writer,
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
              <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
              <Default Extension="xml" ContentType="application/xml"/>
              <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
              <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
            </Types>"#,
            options,
        );
        write_xlsx_fixture(
            &mut writer,
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
              <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
            </Relationships>"#,
            options,
        );
        write_xlsx_fixture(
            &mut writer,
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
              xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
              <sheets>
                <sheet name="Inventory" sheetId="1" r:id="rId1"/>
              </sheets>
            </workbook>"#,
            options,
        );
        write_xlsx_fixture(
            &mut writer,
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
              <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
            </Relationships>"#,
            options,
        );
        write_xlsx_fixture(
            &mut writer,
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
              <dimension ref="A1:B2"/>
              <sheetData>
                <row r="1">
                  <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
                  <c r="B1" t="inlineStr"><is><t>Value</t></is></c>
                </row>
                <row r="2">
                  <c r="A2" t="inlineStr"><is><t>Alpha</t></is></c>
                  <c r="B2"><v>42</v></c>
                </row>
              </sheetData>
            </worksheet>"#,
            options,
        );

        writer.finish().expect("finish xlsx").into_inner()
    }

    fn write_xlsx_fixture(
        writer: &mut ZipWriter<Cursor<Vec<u8>>>,
        path: &str,
        body: &str,
        options: SimpleFileOptions,
    ) {
        writer.start_file(path, options).expect("start xlsx fixture file");
        writer.write_all(body.as_bytes()).expect("write xlsx fixture file");
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
    fn still_rejects_explicit_unsupported_mime_type() {
        assert_eq!(
            detect_upload_file_kind(
                Some("clip.mp4"),
                Some("video/mp4"),
                b"plain text payload\n",
            ),
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
    fn builds_pdf_extraction_plan_for_minimal_pdf_upload() {
        let plan = match build_file_extraction_plan(
            Some("manual.pdf"),
            Some("application/pdf"),
            build_minimal_pdf_bytes(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("pdf extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::Pdf);
        assert_eq!(plan.extraction_kind, "pdf_text");
        assert_eq!(plan.source_format_metadata.page_count, Some(1));
        assert!(
            plan.normalized_text
                .as_deref()
                .is_some_and(|text| text.contains("Quarterly graph report"))
        );
        assert!(plan.structure_hints.lines.iter().any(|line| line.page_number == Some(1)));
    }

    #[test]
    fn builds_spreadsheet_extraction_plan_for_minimal_xlsx_upload() {
        let plan = match build_file_extraction_plan(
            Some("inventory.xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            build_minimal_xlsx_bytes(),
        ) {
            Ok(plan) => plan,
            Err(error) => panic!("spreadsheet extraction plan: {error}"),
        };

        assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
        assert_eq!(plan.extraction_kind, "tabular_text");
        assert_eq!(plan.source_format_metadata.source_format, "xlsx");
        assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("# Inventory")));
        assert!(
            plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Alpha | 42 |"))
        );
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
            &build_minimal_xlsx_bytes(),
        );

        assert_eq!(
            result.expect("spreadsheet upload should be admitted"),
            UploadFileKind::Spreadsheet
        );
    }

    #[tokio::test]
    async fn runtime_plan_uses_vision_provider_for_images() {
        let provider = ProviderModelSelection {
            provider_kind: crate::domains::provider_profiles::SupportedProviderKind::OpenAi,
            model_name: "gpt-5.4-mini".to_string(),
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
    }

    #[test]
    fn builds_truncated_content_preview_without_mutating_body() {
        let preview = build_extracted_content_preview(Some("Alpha Beta Gamma"), 5);

        assert_eq!(preview.text.as_deref(), Some("Alpha"));
        assert!(preview.truncated);
    }
}
