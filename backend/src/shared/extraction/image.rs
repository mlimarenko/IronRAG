use std::io::Cursor;

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat, imageops::FilterType};

use crate::{
    integrations::llm::{LlmGateway, VisionRequest},
    shared::extraction::ExtractionOutput,
};

pub async fn extract_image_with_provider(
    gateway: &dyn LlmGateway,
    provider_kind: &str,
    model_name: &str,
    api_key: &str,
    base_url: Option<&str>,
    mime_type: &str,
    file_bytes: &[u8],
) -> Result<ExtractionOutput> {
    let normalized_payload = prepare_vision_image_payload(file_bytes, mime_type).context(
        "image payload could not be decoded and normalized for vision extraction; re-export as PNG/JPEG and retry",
    )?;
    let request_image_bytes = normalized_payload.image_bytes;
    let request_mime_type = normalized_payload.mime_type;
    let warnings = normalized_payload.warnings;

    let response = gateway
        .vision_extract(VisionRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: "Return only the text visible in this image as plain text. Do not add headings, explanations, summaries, entity lists, markdown, or quotes. If no readable text is visible, return an empty string."
                .to_string(),
            image_bytes: request_image_bytes,
            mime_type: request_mime_type.clone(),
            api_key_override: Some(api_key.to_string()),
            base_url_override: base_url.map(str::to_string),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        })
        .await?;

    Ok(ExtractionOutput {
        extraction_kind: "vision_image".into(),
        content_text: response.output_text,
        page_count: Some(1),
        warnings,
        source_map: serde_json::json!({
            "mime_type": request_mime_type,
        }),
        provider_kind: Some(response.provider_kind),
        model_name: Some(response.model_name),
    })
}

struct PreparedVisionPayload {
    image_bytes: Vec<u8>,
    mime_type: String,
    warnings: Vec<String>,
}

fn prepare_vision_image_payload(
    file_bytes: &[u8],
    mime_type: &str,
) -> Result<PreparedVisionPayload> {
    let mut image = image::load_from_memory(file_bytes).context("failed to decode image bytes")?;
    let mut warnings = Vec::new();

    let width = image.width();
    let height = image.height();
    const MIN_DIMENSION: u32 = 64;
    if width < MIN_DIMENSION || height < MIN_DIMENSION {
        let target_width = width.max(MIN_DIMENSION);
        let target_height = height.max(MIN_DIMENSION);
        image = image.resize_exact(target_width, target_height, FilterType::Triangle);
        warnings.push(format!(
            "upscaled image from {}x{} to {}x{} for provider compatibility",
            width, height, target_width, target_height
        ));
    }

    // Normalize to opaque RGB so providers receive a consistent payload shape.
    let image = DynamicImage::ImageRgb8(image.to_rgb8());
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, ImageFormat::Png)
        .context("failed to encode normalized png payload")?;

    if !mime_type.eq_ignore_ascii_case("image/png") {
        warnings.push(format!(
            "normalized image payload from {mime_type} to image/png for provider compatibility"
        ));
    }

    Ok(PreparedVisionPayload {
        image_bytes: cursor.into_inner(),
        mime_type: "image/png".to_string(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use anyhow::Result;
    use async_trait::async_trait;
    use image::{DynamicImage, ImageFormat};

    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionResponse,
    };

    struct FakeGateway;

    fn valid_png_bytes() -> Vec<u8> {
        let image = DynamicImage::new_rgba8(2, 2);
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).expect("encode generated png fixture");
        cursor.into_inner()
    }

    #[async_trait]
    impl LlmGateway for FakeGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            unreachable!("generate is not used in image extraction tests")
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in image extraction tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in image extraction tests")
        }

        async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
            Ok(VisionResponse {
                provider_kind: request.provider_kind,
                model_name: request.model_name,
                output_text: format!("diagram text and entities [{}]", request.mime_type),
                usage_json: serde_json::json!({}),
            })
        }
    }

    #[tokio::test]
    async fn normalizes_provider_vision_response() {
        let output = extract_image_with_provider(
            &FakeGateway,
            "openai",
            "gpt-5-mini",
            "test-key",
            None,
            "image/png",
            &valid_png_bytes(),
        )
        .await
        .expect("image extraction");

        assert_eq!(output.extraction_kind, "vision_image");
        assert_eq!(output.page_count, Some(1));
        assert_eq!(output.provider_kind.as_deref(), Some("openai"));
        assert_eq!(output.model_name.as_deref(), Some("gpt-5-mini"));
        assert!(output.content_text.contains("diagram text"));
    }

    #[tokio::test]
    async fn normalizes_non_png_mime_payloads_before_provider_call() {
        let output = extract_image_with_provider(
            &FakeGateway,
            "openai",
            "gpt-5-mini",
            "test-key",
            None,
            "image/webp",
            &valid_png_bytes(),
        )
        .await
        .expect("image extraction");

        assert_eq!(output.source_map["mime_type"], serde_json::json!("image/png"));
        assert!(output.warnings.len() >= 1);
        assert!(output.content_text.contains("[image/png]"));
    }
}
