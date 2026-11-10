mod errors;
mod kind;
mod mime_detection;
mod normalization;
mod visual_recognition;

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
    integrations::{llm::LlmGateway, xberg},
    shared::extraction::{
        self, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
    },
};

use self::kind::lower_file_extension;
use self::normalization::{normalize_extracted_content, with_extraction_quality_markers};
pub(crate) use self::visual_recognition::aggregate_vision_usage_json;
pub use self::visual_recognition::{
    VisualRecognitionInput, VisualRecognitionReceipt, recognize_native_visuals,
};
pub use self::{
    errors::{
        ExtractionFailureClass, FileExtractError, MultipartFileBodyErrorKind, UploadAdmissionError,
        UploadRejectionDetails, multipart_file_body_error,
    },
    kind::UploadFileKind,
    mime_detection::{
        detect_declared_upload_file_kind, detect_upload_file_kind, validate_upload_file_admission,
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
    pub vision_dispatch: Option<&'a crate::shared::provider_http::DispatchAuthority>,
    pub max_output_tokens_override: Option<i32>,
    pub cancellation_token: Option<&'a tokio_util::sync::CancellationToken>,
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

pub(crate) fn xberg_failure_class(error: &xberg::XbergExtractionError) -> ExtractionFailureClass {
    if error.memory_failure_code().is_some() {
        return ExtractionFailureClass::OperationalTerminal;
    }
    match error {
        // Invalid extractor output says nothing conclusive about the input
        // bytes. Retrying unchanged output is wasteful, but permanently
        // rejecting the content would discard a recoverable document.
        xberg::XbergExtractionError::InvalidUtf8(_)
        | xberg::XbergExtractionError::InvalidJson(_)
        | xberg::XbergExtractionError::EmptyOutput
        | xberg::XbergExtractionError::ProducerRejected { .. }
        | xberg::XbergExtractionError::Contract(_)
        | xberg::XbergExtractionError::Configuration(_) => {
            ExtractionFailureClass::OperationalTerminal
        }
        xberg::XbergExtractionError::PageExtractionFailed { source, .. } => {
            xberg_failure_class(source)
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
                .filter(|item| {
                    item.get("recognition_capability").and_then(serde_json::Value::as_str)
                        == Some("image_ocr")
                })
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
            } else {
                return Err(runtime_xberg_required_error(file_kind));
            };
            Ok(build_plan_from_extraction(
                file_kind,
                output,
                deterministic_recognition_profile(file_kind),
            ))
        }
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Err(runtime_xberg_required_error(file_kind))
        }
        UploadFileKind::Image if xberg_source_format(file_kind, file_name, mime_type).is_some() => {
            Err(runtime_xberg_required_error(file_kind))
        }
        UploadFileKind::Image => Err(FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::OperationalTerminal,
            message: "image extraction requires a runtime provider context".to_string(),
        }),
        UploadFileKind::Spreadsheet
            if extraction::tabular::native_tabular_mime(file_name, mime_type, file_bytes)
                .is_none() =>
        {
            Ok(build_plan_from_extraction(
                file_kind,
                extraction::tabular::extract_tabular(file_name, mime_type, file_bytes, None)
                    .map_err(|error| FileExtractError::ExtractionFailed {
                        file_kind,
                        // The typed tabular error carries its own verdict, so the
                        // class is never inferred from a message or a fallback.
                        class: error.failure_class(),
                        message: error.to_string(),
                    })?,
                deterministic_recognition_profile(file_kind),
            ))
        }
        UploadFileKind::Spreadsheet => Err(runtime_xberg_required_error(file_kind)),
        UploadFileKind::Binary => Err(FileExtractError::UnsupportedBinary),
    }
}

/// Runs general formats through the native child, then applies only product
/// projections. Record semantics and declared XLSX Tables remain local owners.
pub async fn build_model_free_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, &bytes);
    if let Some(source_format) = xberg_source_format(file_kind, file_name, mime_type) {
        let source = xberg::XbergSource { file_name, mime_type, source_format };
        let native = xberg::extract_native(source, bytes).await.map_err(|error| {
            FileExtractError::ExtractionFailed {
                file_kind,
                class: xberg_failure_class(&error),
                message: error.to_string(),
            }
        })?;
        let output = xberg::project_native(native, source).map_err(|error| {
            FileExtractError::ExtractionFailed {
                file_kind,
                class: xberg_failure_class(&error),
                message: error.to_string(),
            }
        })?;
        return Ok(build_plan_from_extraction(
            file_kind,
            output,
            xberg_owned_recognition_profile(file_kind, source_format)?,
        ));
    }
    let tabular_mime = (file_kind == UploadFileKind::Spreadsheet)
        .then(|| extraction::tabular::native_tabular_mime(file_name, mime_type, &bytes))
        .flatten();
    let local = build_local_file_extraction_plan(file_name, mime_type, &bytes);
    if local.is_ok()
        || !matches!(file_kind, UploadFileKind::TextLike | UploadFileKind::Spreadsheet)
        || declared_payload_is_record_jsonl(file_name, mime_type)
        || (file_kind == UploadFileKind::Spreadsheet && tabular_mime.is_none())
    {
        return local;
    }
    let html = file_kind == UploadFileKind::TextLike
        && (mime_detection::declared_payload_is_html(file_name, mime_type)
            || mime_detection::payload_looks_like_html(&bytes));
    if !html
        && (file_kind == UploadFileKind::TextLike
            || matches!(tabular_mime, Some("text/csv" | "text/tab-separated-values")))
        && std::str::from_utf8(&bytes).is_err()
    {
        if file_kind == UploadFileKind::Spreadsheet {
            return Err(FileExtractError::ExtractionFailed {
                file_kind,
                class: ExtractionFailureClass::ContentInvalid,
                message: "delimited table input is not valid UTF-8".into(),
            });
        }
        return Err(FileExtractError::InvalidUtf8);
    }
    let extension = lower_file_extension(file_name).unwrap_or_else(|| "txt".into());
    let source_format = if html {
        "html"
    } else {
        match extension.as_str() {
            "md" | "markdown" => "md",
            "rst" => "rst",
            "xml" => "xml",
            "svg" => "svg",
            "csv" => "csv",
            "tsv" => "tsv",
            "xls" => "xls",
            "xlsb" => "xlsb",
            "ods" => "ods",
            _ => "txt",
        }
    };
    let html_mime = mime_type.and_then(|mime| mime.split_once(';')).map_or_else(
        || "text/html".to_owned(),
        |(_, parameters)| format!("text/html;{parameters}"),
    );
    let resolved_mime = tabular_mime.unwrap_or(if html {
        &html_mime
    } else {
        match source_format {
            "md" => "text/markdown",
            "rst" => "text/x-rst",
            _ => "text/plain",
        }
    });
    let source = xberg::XbergSource { file_name, mime_type: Some(resolved_mime), source_format };
    let native = xberg::extract_native(source, bytes.clone()).await.map_err(|error| {
        FileExtractError::ExtractionFailed {
            file_kind,
            class: xberg_failure_class(&error),
            message: error.to_string(),
        }
    })?;
    let mut output = if file_kind == UploadFileKind::Spreadsheet {
        extraction::tabular::extract_tabular(file_name, mime_type, &bytes, Some(&native)).map_err(
            |error| FileExtractError::ExtractionFailed {
                file_kind,
                class: error.failure_class(),
                message: error.to_string(),
            },
        )?
    } else if html {
        extraction::html_main_content::extract_html_main_content(&bytes, mime_type, &native.content)
            .map_err(|error| FileExtractError::ExtractionFailed {
                file_kind,
                class: ExtractionFailureClass::OperationalTerminal,
                message: error.to_string(),
            })?
    } else {
        extraction::text_like::extract_text_like(&native.content).map_err(|error| {
            FileExtractError::ExtractionFailed {
                file_kind,
                class: ExtractionFailureClass::OperationalTerminal,
                message: error.to_string(),
            }
        })?
    };
    let provenance = xberg::project_native(native, source).map_err(|error| {
        FileExtractError::ExtractionFailed {
            file_kind,
            class: xberg_failure_class(&error),
            message: error.to_string(),
        }
    })?;
    output.warnings.extend(provenance.warnings);
    output.source_map["native_extraction"] = provenance.source_map;
    Ok(build_plan_from_extraction(file_kind, output, deterministic_recognition_profile(file_kind)))
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
        vision_dispatch,
        max_output_tokens_override,
        cancellation_token,
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

    let input = VisualRecognitionInput {
        dispatch: vision_dispatch,
        max_output_tokens_override,
        gateway,
        vision_provider,
        api_key: vision_api_key.unwrap_or_default(),
        base_url: vision_base_url,
        extra_parameters_json: vision_extra_parameters_json,
        cancellation_token,
    };
    let _ = recognition_policy; // The policy domain validates the singleton Vision policy.
    if let Some(source_format) = xberg_source_format(file_kind, file_name, mime_type) {
        let source = xberg::XbergSource { file_name, mime_type, source_format };
        let mut native = xberg::extract_native(source, file_bytes).await.map_err(|error| {
            FileExtractError::ExtractionFailed {
                file_kind,
                class: xberg_failure_class(&error),
                message: error.to_string(),
            }
        })?;
        let receipt = recognize_native_visuals(&mut native, file_kind, input).await?;
        let calls = receipt.calls.clone();
        let mut output =
            xberg::project_native(native, source).map_err(|_| FileExtractError::VisionFailed {
                file_kind,
                class: ExtractionFailureClass::OperationalTerminal,
                message: "native extraction projection failed after visual recognition".into(),
                calls: calls.clone(),
            })?;
        receipt.apply(&mut output);
        if output.content_text.trim().is_empty() {
            return Err(FileExtractError::VisionFailed {
                file_kind,
                class: ExtractionFailureClass::ContentInvalid,
                message: "document contains no usable native or recognized text".into(),
                calls,
            });
        }
        return Ok(build_plan_from_extraction(
            file_kind,
            output,
            xberg_owned_recognition_profile(file_kind, source_format)?,
        ));
    }
    if file_kind == UploadFileKind::Image {
        let provider = vision_provider.ok_or_else(|| missing_vision_binding_error(file_kind))?;
        let empty = serde_json::json!({});
        let output = extraction::image::extract_image_with_provider(
            gateway,
            extraction::image::VisionProviderInput {
                dispatch: vision_dispatch,
                max_output_tokens_override,
                provider_kind: &provider.provider_kind,
                model_name: &provider.model_name,
                api_key: input.api_key,
                base_url: vision_base_url,
                extra_parameters_json: vision_extra_parameters_json.unwrap_or(&empty),
            },
            mime_type.unwrap_or("image/png"),
            &file_bytes,
            cancellation_token,
        )
        .await
        .map_err(|error| FileExtractError::VisionFailed {
            file_kind,
            class: if error.terminal {
                ExtractionFailureClass::OperationalTerminal
            } else {
                ExtractionFailureClass::OperationalRetryable
            },
            message: error.message.into(),
            calls: error.calls,
        })?;
        if output.content_text.trim().is_empty() {
            let calls = serde_json::from_value(output.usage_json["vision_calls"].clone())
                .unwrap_or_default();
            return Err(FileExtractError::VisionFailed {
                file_kind,
                class: ExtractionFailureClass::ContentInvalid,
                message: "image contains no readable text".into(),
                calls,
            });
        }
        return Ok(build_plan_from_extraction(
            file_kind,
            output,
            RecognitionProfile {
                capability: RecognitionCapability::ImageOcr,
                engine: RecognitionEngine::Vision,
                structure_tier: RecognitionStructureTier::Flat,
            },
        ));
    }
    build_model_free_file_extraction_plan(file_name, mime_type, file_bytes).await
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
        RecognitionProfile {
            capability: RecognitionCapability::TextDecode,
            engine: RecognitionEngine::Native,
            structure_tier: RecognitionStructureTier::Paragraph,
        },
    )
}

/// Builds an extraction plan for inline text when the caller knows the logical
/// file name or MIME type. This keeps append/edit materialization on the same
/// deterministic adapter path as uploads.
pub async fn build_inline_text_extraction_plan_for_source(
    text: &str,
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Result<FileExtractionPlan, FileExtractError> {
    build_model_free_file_extraction_plan(file_name, mime_type, text.as_bytes().to_vec()).await
}

pub(crate) fn build_plan_from_extraction(
    file_kind: UploadFileKind,
    output: ExtractionOutput,
    mut recognition_profile: RecognitionProfile,
) -> FileExtractionPlan {
    if output.extraction_kind == "record_jsonl" || output.source_metadata.source_format == "xlsx" {
        recognition_profile.engine = RecognitionEngine::Native;
    }
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

#[must_use]
pub fn build_xberg_pdf_extraction_plan(output: ExtractionOutput) -> FileExtractionPlan {
    build_plan_from_extraction(
        UploadFileKind::Pdf,
        output,
        RecognitionProfile {
            capability: RecognitionCapability::DocumentLayout,
            engine: RecognitionEngine::Xberg,
            structure_tier: RecognitionStructureTier::Layout,
        },
    )
}

const fn deterministic_recognition_profile(file_kind: UploadFileKind) -> RecognitionProfile {
    match file_kind {
        UploadFileKind::Spreadsheet => RecognitionProfile {
            capability: RecognitionCapability::TabularParse,
            engine: RecognitionEngine::Xberg,
            structure_tier: RecognitionStructureTier::Layout,
        },
        _ => RecognitionProfile {
            capability: RecognitionCapability::TextDecode,
            engine: RecognitionEngine::Xberg,
            structure_tier: RecognitionStructureTier::Paragraph,
        },
    }
}

pub(crate) fn xberg_owned_recognition_profile(
    file_kind: UploadFileKind,
    source_format: &str,
) -> Result<RecognitionProfile, FileExtractError> {
    match file_kind {
        UploadFileKind::Pdf | UploadFileKind::Docx | UploadFileKind::Pptx => {
            Ok(RecognitionProfile {
                capability: RecognitionCapability::DocumentLayout,
                engine: RecognitionEngine::Xberg,
                structure_tier: RecognitionStructureTier::Layout,
            })
        }
        UploadFileKind::Image => Ok(RecognitionProfile {
            capability: RecognitionCapability::ImageOcr,
            engine: RecognitionEngine::Vision,
            structure_tier: RecognitionStructureTier::Layout,
        }),
        _ => Err(FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::OperationalTerminal,
            message: format!("native source format {source_format} is not valid for {file_kind:?}"),
        }),
    }
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
        "xberg" => "xberg".to_string(),
        "vision" => "vision_llm".to_string(),
        other => other.to_string(),
    }
}

fn missing_vision_binding_error(file_kind: UploadFileKind) -> FileExtractError {
    FileExtractError::ExtractionFailed {
        file_kind,
        class: ExtractionFailureClass::OperationalTerminal,
        message: "explicit Vision recognition requires an active extract_text binding; local OCR is not a fallback".into(),
    }
}

fn runtime_xberg_required_error(file_kind: UploadFileKind) -> FileExtractError {
    FileExtractError::ExtractionFailed {
        file_kind,
        // The file kind needs the runtime xberg path; the deterministic local
        // path is the wrong adapter, which no retry of the same call fixes.
        class: ExtractionFailureClass::OperationalTerminal,
        message: format!(
            "xberg runtime extraction is required for {} uploads",
            file_kind.display_name()
        ),
    }
}

pub(crate) fn xberg_source_format(
    file_kind: UploadFileKind,
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<&'static str> {
    match file_kind {
        UploadFileKind::Pdf => Some("pdf"),
        UploadFileKind::Docx => Some("docx"),
        UploadFileKind::Pptx => Some("pptx"),
        UploadFileKind::Image => xberg_image_source_format(file_name, mime_type),
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

fn xberg_image_source_format(
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
