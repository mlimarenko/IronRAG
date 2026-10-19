mod errors;
mod kind;
mod mime_detection;
mod normalization;
mod picture_ocr;

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

use self::kind::lower_file_extension;
use self::normalization::{normalize_extracted_content, with_extraction_quality_markers};
pub(crate) use self::picture_ocr::aggregate_vision_picture_usage_json;
use self::picture_ocr::{augment_with_vision_picture_ocr, extract_image_with_vision_provider};
pub use self::{
    errors::{
        ExtractionFailureClass, FileExtractError, MultipartFileBodyErrorKind, UploadAdmissionError,
        UploadRejectionDetails, multipart_file_body_error,
    },
    kind::UploadFileKind,
    mime_detection::{
        detect_declared_upload_file_kind, detect_upload_file_kind, validate_upload_file_admission,
    },
    picture_ocr::{
        VisionOcrInput, append_docling_picture_ocr_fallback,
        augment_docling_output_with_vision_picture_ocr, docling_embedded_picture_vision_enabled,
        raster_image_vision_enabled,
    },
};

pub const MULTIPART_UPLOAD_MODE: &str = "multipart_upload_v2";
const EXTRACTION_QUALITY_KEY: &str = "content_quality";

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
    pub vision_extra_parameters_json: Option<&'a serde_json::Value>,
    pub file_name: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub file_bytes: Vec<u8>,
    pub recognition_policy: &'a LibraryRecognitionPolicy,
}

pub(crate) fn docling_failure_class(
    error: &docling::DoclingExtractionError,
) -> ExtractionFailureClass {
    if error.memory_failure_code().is_some() {
        return ExtractionFailureClass::OperationalTerminal;
    }
    match error {
        // Invalid extractor output says nothing conclusive about the input
        // bytes. Retrying unchanged output is wasteful, but permanently
        // rejecting the content would discard a recoverable document.
        docling::DoclingExtractionError::InvalidUtf8(_)
        | docling::DoclingExtractionError::InvalidJson(_)
        | docling::DoclingExtractionError::EmptyOutput => {
            ExtractionFailureClass::OperationalTerminal
        }
        docling::DoclingExtractionError::PageExtractionFailed { source, .. } => {
            docling_failure_class(source)
        }
        _ => ExtractionFailureClass::OperationalRetryable,
    }
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

/// Reads extraction quality markers from a source map and extraction metadata.
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
                    FileExtractError::ExtractionFailed {
                        file_kind,
                        class: ExtractionFailureClass::ContentInvalid,
                        message: error.to_string(),
                    }
                })?
            } else if let Some(record_output) =
                extraction::record_jsonl::structured_record_hint(file_name, mime_type).and_then(
                    |hint| extraction::record_jsonl::normalize_structured_records(file_bytes, hint),
                )
            {
                // Plain JSON (object/array) and YAML (doc/`---` stream) route
                // through the same record renderer as JSONL. Non-record-shaped
                // payloads return None above and fall through to the text path.
                record_output
            } else if mime_detection::declared_payload_is_html(file_name, mime_type)
                || mime_detection::payload_looks_like_html(file_bytes)
            {
                extraction::html_main_content::extract_html_main_content(file_bytes, mime_type)
                    .map_err(|error| FileExtractError::ExtractionFailed {
                        file_kind,
                        class: ExtractionFailureClass::ContentInvalid,
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
            class: ExtractionFailureClass::OperationalTerminal,
            message: "image extraction requires a runtime provider context".to_string(),
        }),
        UploadFileKind::Spreadsheet => Ok(build_plan_from_extraction(
            file_kind,
            extraction::tabular::extract_tabular(file_name, mime_type, file_bytes).map_err(
                |error| FileExtractError::ExtractionFailed {
                    file_kind,
                    // The typed tabular error carries its own verdict, so the
                    // class is never inferred from a message or a fallback.
                    class: error.failure_class(),
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
        vision_extra_parameters_json,
        file_name,
        mime_type,
        file_bytes,
        recognition_policy,
    } = request;
    let file_kind = detect_upload_file_kind(file_name, mime_type, &file_bytes);

    if let Some(source_format) = docling_source_format(file_kind, file_name, mime_type) {
        let vision_binding_available = vision_provider.is_some();
        let recognition_profile = docling_owned_recognition_profile(
            file_kind,
            source_format,
            recognition_policy,
            vision_binding_available,
        )?;
        return match recognition_profile.engine {
            RecognitionEngine::Docling => {
                let mut output = docling::extract_document(
                    docling::DoclingSource { file_name, mime_type, source_format },
                    file_bytes,
                )
                .await
                .map_err(|error| FileExtractError::ExtractionFailed {
                    file_kind,
                    // Extractor failures cannot prove the source bytes
                    // unsupported, even when its output is malformed.
                    class: docling_failure_class(&error),
                    message: error.to_string(),
                })?;
                if docling_embedded_picture_vision_enabled(
                    recognition_policy,
                    vision_binding_available,
                ) {
                    augment_with_vision_picture_ocr(
                        &mut output,
                        file_kind,
                        VisionOcrInput {
                            gateway,
                            vision_provider,
                            api_key: vision_api_key,
                            base_url: vision_base_url,
                            extra_parameters_json: vision_extra_parameters_json,
                        },
                    )
                    .await?;
                } else {
                    append_docling_picture_ocr_fallback(&mut output);
                    output.extracted_images.clear();
                }
                Ok(build_plan_from_extraction(file_kind, output, recognition_profile))
            }
            RecognitionEngine::Vision => {
                extract_image_with_vision_provider(
                    VisionOcrInput {
                        gateway,
                        vision_provider,
                        api_key: vision_api_key,
                        base_url: vision_base_url,
                        extra_parameters_json: vision_extra_parameters_json,
                    },
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
                VisionOcrInput {
                    gateway,
                    vision_provider,
                    api_key: vision_api_key,
                    base_url: vision_base_url,
                    extra_parameters_json: vision_extra_parameters_json,
                },
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
                // A panicked blocking extraction task is a worker fault, not a
                // statement about the payload.
                class: ExtractionFailureClass::OperationalRetryable,
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

    let normalized = normalize_extracted_content(
        file_kind,
        recognition_profile.engine,
        &content_text,
        &structure_hints,
    );
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

#[must_use]
pub fn build_docling_pdf_extraction_plan(output: ExtractionOutput) -> FileExtractionPlan {
    build_plan_from_extraction(
        UploadFileKind::Pdf,
        output,
        RecognitionProfile {
            capability: RecognitionCapability::DocumentLayout,
            engine: RecognitionEngine::Docling,
            structure_tier: RecognitionStructureTier::Layout,
        },
    )
}

const fn deterministic_recognition_profile(file_kind: UploadFileKind) -> RecognitionProfile {
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
    vision_binding_available: bool,
) -> Result<RecognitionProfile, FileExtractError> {
    match file_kind {
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Ok(RecognitionProfile {
                capability: RecognitionCapability::DocumentLayout,
                engine: RecognitionEngine::Docling,
                structure_tier: RecognitionStructureTier::Layout,
            })
        }
        UploadFileKind::Image
            if raster_image_vision_enabled(recognition_policy, vision_binding_available) =>
        {
            Ok(RecognitionProfile {
                capability: RecognitionCapability::ImageOcr,
                engine: RecognitionEngine::Vision,
                structure_tier: RecognitionStructureTier::Flat,
            })
        }
        UploadFileKind::Image => match recognition_policy.raster_image_engine {
            RecognitionEngine::Docling | RecognitionEngine::Vision => Ok(RecognitionProfile {
                capability: RecognitionCapability::ImageOcr,
                engine: RecognitionEngine::Docling,
                structure_tier: RecognitionStructureTier::Layout,
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
                // A recognition-policy/source-format mismatch is an operator
                // configuration fault, not a rejected payload.
                class: ExtractionFailureClass::OperationalTerminal,
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
        // A recognition engine that cannot serve the capability is an operator
        // policy gap; the payload is not at fault.
        class: ExtractionFailureClass::OperationalTerminal,
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
        // The file kind needs the runtime docling path; the deterministic local
        // path is the wrong adapter, which no retry of the same call fixes.
        class: ExtractionFailureClass::OperationalTerminal,
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

#[cfg(test)]
mod tests;
