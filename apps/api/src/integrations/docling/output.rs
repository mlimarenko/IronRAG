use super::*;

use base64::Engine as _;

pub(super) fn build_output(
    payload: DoclingExtractionPayload,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let has_picture_payload = !payload.pictures.is_empty();
    let has_picture_ocr_text =
        payload.picture_ocr_text.iter().any(|snippet| !snippet.trim().is_empty());
    let content = select_docling_content(payload.markdown, payload.text);
    if content.trim().is_empty() && !has_picture_payload && !has_picture_ocr_text {
        return Err(DoclingExtractionError::EmptyOutput);
    }

    let layout = build_text_layout_from_content(content.trim());
    let line_count = i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX);
    let input_format = payload.input_format.unwrap_or_else(|| source_format.to_string());
    let page_count = payload.page_count;

    let extracted_images = payload
        .pictures
        .into_iter()
        .filter_map(|picture| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(picture.content_base64.as_bytes())
                .ok()?;
            let width = picture.size_px.first().copied().unwrap_or_default();
            let height = picture.size_px.get(1).copied().unwrap_or_default();
            Some(crate::shared::extraction::ExtractedImage {
                page: 0,
                image_bytes: bytes,
                mime_type: picture.mime.unwrap_or_else(|| "image/png".to_string()),
                width,
                height,
            })
        })
        .collect();

    Ok(ExtractionOutput {
        extraction_kind: "docling_markdown".to_string(),
        content_text: layout.content_text,
        page_count,
        warnings: payload.warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: input_format.clone(),
            page_count,
            line_count,
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "adapter": "docling",
            "input_file_name": file_name,
            "mime_type": mime_type,
            "source_format": source_format,
            "docling_input_format": input_format,
            "docling_status": payload.status,
            "docling_version": payload.docling_version,
            "docling_picture_ocr_text": payload.picture_ocr_text,
            "timings": payload.timings,
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images,
    })
}

fn select_docling_content(markdown: String, text: Option<String>) -> String {
    let text = text.unwrap_or_default();
    let markdown_body = markdown.replace("<!-- image -->", "");
    if markdown_body.trim().is_empty() || markdown.trim().is_empty() { text } else { markdown }
}
