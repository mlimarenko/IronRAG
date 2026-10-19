use crate::{
    domains::{
        provider_profiles::ProviderModelSelection,
        recognition::{LibraryRecognitionPolicy, RecognitionEngine, RecognitionProfile},
    },
    integrations::llm::LlmGateway,
    shared::extraction::{self as extraction, ExtractedImage, ExtractionOutput},
};

use super::{
    ExtractionFailureClass, FileExtractError, FileExtractionPlan, UploadFileKind,
    build_plan_from_extraction,
};

/// One embedded-picture OCR pass: the gateway plus the resolved
/// document-understanding binding (provider selection, credential override, base
/// URL, and extra request parameters). Grouped so the entry points stop
/// threading five provider fields positionally.
pub struct VisionOcrInput<'a> {
    pub gateway: &'a dyn LlmGateway,
    pub vision_provider: Option<&'a ProviderModelSelection>,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub extra_parameters_json: Option<&'a serde_json::Value>,
}

/// Run the active document-understanding binding over every embedded picture
/// reported by Docling, then append the per-picture OCR text to the
/// extraction's `content_text` so chunking, embedding, and graph
/// extraction see it.
///
pub(super) async fn augment_with_vision_picture_ocr(
    output: &mut ExtractionOutput,
    file_kind: UploadFileKind,
    input: VisionOcrInput<'_>,
) -> Result<(), FileExtractError> {
    let VisionOcrInput { gateway, vision_provider, api_key, base_url, extra_parameters_json } =
        input;
    tracing::debug!(
        extracted_images = output.extracted_images.len(),
        vision_provider = ?vision_provider.map(|vp| (vp.provider_kind.as_str(), vp.model_name.as_str())),
        "augment_with_vision_picture_ocr entry"
    );
    let extracted_images = std::mem::take(&mut output.extracted_images);
    if extracted_images.is_empty() {
        append_docling_picture_ocr_fallback(output);
        return Ok(());
    }
    let Some(vision_provider) = vision_provider else {
        return Err(FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::OperationalTerminal,
            message: "document-understanding binding is not configured for embedded picture OCR"
                .to_string(),
        });
    };
    let empty_extra_parameters = serde_json::json!({});
    let extra_parameters_json = extra_parameters_json.unwrap_or(&empty_extra_parameters);
    let mut snippets: Vec<String> = Vec::with_capacity(extracted_images.len());
    let image_count = extracted_images.len();
    let mut usage_items = Vec::with_capacity(image_count);
    let mut failed_picture_count = 0usize;
    for (idx, image) in extracted_images.into_iter().enumerate() {
        match extract_picture_ocr(
            gateway,
            extraction::image::VisionProviderInput {
                provider_kind: vision_provider.provider_kind.as_str(),
                model_name: &vision_provider.model_name,
                api_key: api_key.unwrap_or_default(),
                base_url,
                extra_parameters_json,
            },
            idx,
            image,
        )
        .await
        {
            Ok((snippet, usage)) => {
                usage_items.push(usage);
                if let Some(snippet) = snippet {
                    snippets.push(snippet);
                }
            }
            Err(error) => {
                failed_picture_count += 1;
                output
                    .warnings
                    .push(format!("vision OCR for embedded picture {} failed: {error}", idx + 1));
            }
        }
    }
    if failed_picture_count == image_count {
        return Err(FileExtractError::ExtractionFailed {
            file_kind,
            // The provider failed for every picture; the payload may still be
            // fine, so a retry can succeed.
            class: ExtractionFailureClass::OperationalRetryable,
            message: format!(
                "vision OCR failed for every embedded picture ({failed_picture_count}/{image_count})"
            ),
        });
    }
    output.provider_kind = Some(vision_provider.provider_kind.clone());
    output.model_name = Some(vision_provider.model_name.clone());
    output.usage_json = aggregate_vision_picture_usage_json(usage_items);
    output.source_map["vision_picture_ocr"] = serde_json::json!({
        "engine": "vision",
        "imageCount": image_count,
        "snippetCount": snippets.len(),
        "failedImageCount": failed_picture_count,
    });
    apply_picture_ocr_content(output, snippets);
    Ok(())
}

async fn extract_picture_ocr(
    gateway: &dyn LlmGateway,
    provider: extraction::image::VisionProviderInput<'_>,
    idx: usize,
    image: ExtractedImage,
) -> Result<(Option<String>, serde_json::Value), anyhow::Error> {
    let mime = if image.mime_type.is_empty() { "image/png" } else { image.mime_type.as_str() };
    let picture_output = extraction::image::extract_image_with_provider(
        gateway,
        provider,
        mime,
        image.image_bytes.as_slice(),
    )
    .await?;
    let trimmed = picture_output.content_text.trim();
    tracing::debug!(
        picture_index = idx,
        snippet_len = trimmed.len(),
        "augment_with_vision_picture_ocr per-picture result"
    );
    let snippet = (!trimmed.is_empty()).then(|| {
        format!(
            "--- Embedded image {} ({}x{}) ---\n{}",
            idx + 1,
            image.width,
            image.height,
            trimmed
        )
    });
    Ok((snippet, picture_output.usage_json))
}

fn apply_picture_ocr_content(output: &mut ExtractionOutput, snippets: Vec<String>) {
    log_picture_ocr_exit_summary(output, snippets.len());
    let stripped_content = strip_docling_picture_ocr_scaffold(&output.content_text);
    let content_changed = stripped_content != output.content_text;
    let snippets_empty = snippets.is_empty();
    if snippets_empty {
        apply_empty_picture_ocr_fallback(output, content_changed, stripped_content);
    } else {
        apply_picture_ocr_snippets(output, snippets, stripped_content);
    }
    if !snippets_empty || content_changed {
        rebuild_picture_ocr_layout(output);
    }
}

fn log_picture_ocr_exit_summary(output: &ExtractionOutput, snippet_count: usize) {
    tracing::debug!(
        snippet_count,
        content_text_len_before = output.content_text.len(),
        "augment_with_vision_picture_ocr exit summary"
    );
}

fn apply_empty_picture_ocr_fallback(
    output: &mut ExtractionOutput,
    content_changed: bool,
    stripped_content: String,
) {
    output.warnings.push(
        "vision OCR returned no embedded picture text; kept Docling picture OCR fallback"
            .to_string(),
    );
    if content_changed && !docling_picture_ocr_fallback_snippets(&output.source_map).is_empty() {
        output.content_text = stripped_content;
        append_docling_picture_ocr_fallback(output);
    }
}

fn apply_picture_ocr_snippets(
    output: &mut ExtractionOutput,
    snippets: Vec<String>,
    stripped_content: String,
) {
    output.content_text = picture_ocr_appended_content(&stripped_content, &snippets);
    tracing::debug!(
        content_text_len_after = output.content_text.len(),
        "augment_with_vision_picture_ocr appended"
    );
}

/// Appends the OCR snippet block to the stripped content, or returns the block
/// alone when stripping left no non-blank content. Pure, so the merge decision is
/// testable without a provider or an [`ExtractionOutput`].
fn picture_ocr_appended_content(stripped_content: &str, snippets: &[String]) -> String {
    let block = snippets.join("\n\n");
    if stripped_content.trim().is_empty() {
        block
    } else {
        format!("{stripped_content}\n\n{block}")
    }
}

fn rebuild_picture_ocr_layout(output: &mut ExtractionOutput) {
    // Rebuild line hints from the augmented content so downstream structured
    // preparation cannot discard OCR lines that were absent from the original layout.
    let layout =
        crate::shared::extraction::build_text_layout_from_content(output.content_text.trim());
    output.content_text = layout.content_text;
    output.structure_hints = layout.structure_hints;
    output.source_metadata.line_count =
        i32::try_from(output.structure_hints.lines.len()).unwrap_or(i32::MAX);
}

pub(crate) fn aggregate_vision_picture_usage_json(
    usages: Vec<serde_json::Value>,
) -> serde_json::Value {
    if usages.is_empty() {
        return serde_json::json!({});
    }

    let mut usage_json = serde_json::json!({
        "embedded_picture_ocr_call_count": usages.len(),
        "embedded_picture_ocr_usage": usages,
    });
    if let Some(prompt_tokens) =
        sum_numeric_usage_key(&usage_json["embedded_picture_ocr_usage"], "prompt_tokens").or_else(
            || sum_numeric_usage_key(&usage_json["embedded_picture_ocr_usage"], "input_tokens"),
        )
    {
        usage_json["prompt_tokens"] = serde_json::json!(prompt_tokens);
    }
    if let Some(completion_tokens) =
        sum_numeric_usage_key(&usage_json["embedded_picture_ocr_usage"], "completion_tokens")
            .or_else(|| {
                sum_numeric_usage_key(&usage_json["embedded_picture_ocr_usage"], "output_tokens")
            })
    {
        usage_json["completion_tokens"] = serde_json::json!(completion_tokens);
    }
    if let Some(total_tokens) =
        sum_numeric_usage_key(&usage_json["embedded_picture_ocr_usage"], "total_tokens")
    {
        usage_json["total_tokens"] = serde_json::json!(total_tokens);
    }
    usage_json
}

fn sum_numeric_usage_key(usages: &serde_json::Value, key: &str) -> Option<i64> {
    let total =
        usages.as_array()?.iter().filter_map(|usage| usage.get(key)).fold(0_i64, |acc, value| {
            acc + value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .unwrap_or_default()
        });
    (total > 0).then_some(total)
}

pub async fn augment_docling_output_with_vision_picture_ocr(
    output: &mut ExtractionOutput,
    file_kind: UploadFileKind,
    input: VisionOcrInput<'_>,
) -> Result<(), FileExtractError> {
    augment_with_vision_picture_ocr(output, file_kind, input).await
}

pub fn append_docling_picture_ocr_fallback(output: &mut ExtractionOutput) {
    let snippets = docling_picture_ocr_fallback_snippets(&output.source_map);
    if snippets.is_empty() {
        return;
    }

    let block = snippets
        .into_iter()
        .enumerate()
        .map(|(idx, snippet)| format!("--- Embedded image {} ---\n{}", idx + 1, snippet))
        .collect::<Vec<_>>()
        .join("\n\n");
    if output.content_text.trim().is_empty() {
        output.content_text = block;
    } else {
        output.content_text = format!("{}\n\n{}", output.content_text, block);
    }
    let layout =
        crate::shared::extraction::build_text_layout_from_content(output.content_text.trim());
    output.content_text = layout.content_text;
    output.structure_hints = layout.structure_hints;
    output.source_metadata.line_count =
        i32::try_from(output.structure_hints.lines.len()).unwrap_or(i32::MAX);
}

fn docling_picture_ocr_fallback_snippets(source_map: &serde_json::Value) -> Vec<String> {
    source_map
        .get("docling_picture_ocr_text")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|snippet| !snippet.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[must_use]
pub fn docling_embedded_picture_vision_enabled(
    recognition_policy: &LibraryRecognitionPolicy,
    vision_binding_available: bool,
) -> bool {
    raster_image_vision_enabled(recognition_policy, vision_binding_available)
}

#[must_use]
pub fn raster_image_vision_enabled(
    recognition_policy: &LibraryRecognitionPolicy,
    vision_binding_available: bool,
) -> bool {
    recognition_policy.raster_image_engine == RecognitionEngine::Vision && vision_binding_available
}

pub(super) async fn extract_image_with_vision_provider(
    input: VisionOcrInput<'_>,
    file_kind: UploadFileKind,
    mime_type: Option<&str>,
    file_bytes: &[u8],
    recognition_profile: RecognitionProfile,
) -> Result<FileExtractionPlan, FileExtractError> {
    let VisionOcrInput { gateway, vision_provider, api_key, base_url, extra_parameters_json } =
        input;
    let Some(vision_provider) = vision_provider else {
        return Err(FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::OperationalTerminal,
            message: "document-understanding binding is not configured for image recognition"
                .to_string(),
        });
    };
    let detected_mime = mime_type.unwrap_or("image/png");
    let empty_extra_parameters = serde_json::json!({});
    let extra_parameters_json = extra_parameters_json.unwrap_or(&empty_extra_parameters);
    let provider = extraction::image::VisionProviderInput {
        provider_kind: vision_provider.provider_kind.as_str(),
        model_name: &vision_provider.model_name,
        api_key: api_key.unwrap_or_default(),
        base_url,
        extra_parameters_json,
    };
    let mut output = extraction::image::extract_image_with_provider(
        gateway,
        provider,
        detected_mime,
        file_bytes,
    )
    .await
    .map_err(|error| FileExtractError::ExtractionFailed {
        file_kind,
        class: ExtractionFailureClass::OperationalRetryable,
        message: error.to_string(),
    })?;
    if output.content_text.trim().is_empty() {
        tracing::warn!(
            provider_kind = vision_provider.provider_kind.as_str(),
            model_name = vision_provider.model_name.as_str(),
            "image recognition returned no readable text on the first attempt; retrying once"
        );
        let first_usage_json = output.usage_json.clone();
        output = extraction::image::extract_image_with_provider(
            gateway,
            provider,
            detected_mime,
            file_bytes,
        )
        .await
        .map_err(|error| FileExtractError::ExtractionFailed {
            file_kind,
            class: ExtractionFailureClass::OperationalRetryable,
            message: error.to_string(),
        })?;
        if output.content_text.trim().is_empty() {
            return Err(FileExtractError::ExtractionFailed {
                file_kind,
                // An HTTP 200 with empty model text twice is a provider/config
                // fault, not proof that the bytes are unreadable: a transient
                // provider failure may clear on a later attempt, so this is
                // operational and never a content rejection.
                class: ExtractionFailureClass::OperationalRetryable,
                message: "image recognition produced no readable text".to_string(),
            });
        }
        output.usage_json = merge_vision_retry_usage(&first_usage_json, &output.usage_json);
        output.warnings.push(
            "image recognition returned no readable text on the first attempt; recovered on retry"
                .to_string(),
        );
    }
    Ok(build_plan_from_extraction(file_kind, output, recognition_profile))
}

/// Sums the token counters of a retried-away empty attempt into the accepted
/// retry response so callers that bill or report the completed stage see both
/// provider calls. Counters absent on a side count as zero; keys absent on both
/// sides stay absent. Non-object `accepted` usage is returned untouched.
fn merge_vision_retry_usage(
    first: &serde_json::Value,
    accepted: &serde_json::Value,
) -> serde_json::Value {
    let serde_json::Value::Object(mut merged) = accepted.clone() else {
        return accepted.clone();
    };
    for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        let first_count = vision_usage_count(first.get(key));
        let accepted_count = vision_usage_count(accepted.get(key));
        if first_count.is_some() || accepted_count.is_some() {
            merged.insert(
                key.to_string(),
                serde_json::Value::from(
                    first_count
                        .unwrap_or_default()
                        .saturating_add(accepted_count.unwrap_or_default()),
                ),
            );
        }
    }
    serde_json::Value::Object(merged)
}

fn vision_usage_count(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| value.as_u64().and_then(|count| i64::try_from(count).ok()))
}

pub(super) fn strip_docling_picture_ocr_scaffold(content_text: &str) -> String {
    let mut applied = false;
    let mut lines = Vec::new();
    let mut blank_run = 0usize;

    for line in content_text.lines() {
        let trimmed = line.trim();
        if trimmed == "<!-- image -->" || trimmed.starts_with("> Image OCR:") {
            applied = true;
            continue;
        }
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                lines.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        lines.push(line.trim_end().to_string());
    }

    if applied { lines.join("\n").trim().to_string() } else { content_text.to_string() }
}

#[cfg(test)]
mod tests {
    use super::picture_ocr_appended_content;

    #[test]
    fn picture_ocr_content_appends_snippets_after_stripped_content() {
        let snippets = vec!["alpha".to_string(), "beta".to_string()];

        assert_eq!(
            picture_ocr_appended_content("# Heading", &snippets),
            "# Heading\n\nalpha\n\nbeta"
        );
    }

    #[test]
    fn picture_ocr_content_uses_snippets_alone_when_stripped_content_is_blank() {
        let snippets = vec!["alpha".to_string()];

        assert_eq!(picture_ocr_appended_content("   \n", &snippets), "alpha");
    }
}
