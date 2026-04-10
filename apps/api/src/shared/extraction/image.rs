use std::io::Cursor;

use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, ImageBuffer, ImageFormat, Luma, LumaA, Rgb, Rgba, imageops::FilterType};
use png::{
    ColorType as PngColorType, Decoder as PngDecoder, Transformations as PngTransformations,
};

use crate::{
    integrations::llm::{LlmGateway, VisionRequest},
    shared::extraction::{
        ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
    },
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
            prompt: "Return only the text visible in this image as plain UTF-8 text, one logical flow with newlines where line breaks are visible. Do not add headings, explanations, summaries, entity lists, markdown fences, or wrapping quotes. If no readable text is visible, return an empty string."
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

    let layout = build_text_layout_from_content(&response.output_text);

    Ok(ExtractionOutput {
        extraction_kind: "vision_image".into(),
        content_text: layout.content_text,
        page_count: Some(1),
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "image".to_string(),
            page_count: Some(1),
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "mime_type": request_mime_type,
        }),
        provider_kind: Some(response.provider_kind),
        model_name: Some(response.model_name),
        extracted_images: Vec::new(),
    })
}

struct PreparedVisionPayload {
    image_bytes: Vec<u8>,
    mime_type: String,
    warnings: Vec<String>,
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn prepare_vision_image_payload(
    file_bytes: &[u8],
    mime_type: &str,
) -> Result<PreparedVisionPayload> {
    let mut image =
        load_normalizable_image(file_bytes, mime_type).context("failed to decode image bytes")?;
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

    // Normalize to opaque RGB on a white matte so transparent assets do not become
    // black canvases when alpha is discarded before provider extraction.
    let image = DynamicImage::ImageRgb8(flatten_to_white_rgb8(&image));
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

fn load_normalizable_image(file_bytes: &[u8], mime_type: &str) -> Result<DynamicImage> {
    if mime_type.eq_ignore_ascii_case("image/png") || file_bytes.starts_with(PNG_SIGNATURE) {
        return decode_png_ignoring_checksums(file_bytes);
    }

    image::load_from_memory(file_bytes).context("failed to decode image bytes with generic decoder")
}

fn flatten_to_white_rgb8(image: &DynamicImage) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgb = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(width, height);

    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = f32::from(pixel[3]) / 255.0;
        let composite = |channel: u8| -> u8 {
            let source = f32::from(channel);
            let white = 255.0;
            ((source * alpha) + (white * (1.0 - alpha))).round() as u8
        };
        rgb.put_pixel(x, y, Rgb([composite(pixel[0]), composite(pixel[1]), composite(pixel[2])]));
    }

    rgb
}

fn decode_png_ignoring_checksums(file_bytes: &[u8]) -> Result<DynamicImage> {
    let mut decoder = PngDecoder::new(Cursor::new(file_bytes));
    decoder.ignore_checksums(true);
    decoder.set_transformations(PngTransformations::normalize_to_color8());

    let mut reader = decoder.read_info().context("failed to read png metadata")?;
    let buffer_size = reader
        .output_buffer_size()
        .context("png decoder could not determine output buffer size")?;
    let mut buffer = vec![0; buffer_size];
    let output = reader.next_frame(&mut buffer).context("failed to decode png frame")?;
    let pixels = buffer[..output.buffer_size()].to_vec();

    match output.color_type {
        PngColorType::Grayscale => {
            ImageBuffer::<Luma<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| anyhow!("failed to construct luma image from decoded png buffer"))
        }
        PngColorType::GrayscaleAlpha => {
            ImageBuffer::<LumaA<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageLumaA8)
                .ok_or_else(|| {
                    anyhow!("failed to construct luma-alpha image from decoded png buffer")
                })
        }
        PngColorType::Rgb => {
            ImageBuffer::<Rgb<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageRgb8)
                .ok_or_else(|| anyhow!("failed to construct rgb image from decoded png buffer"))
        }
        PngColorType::Rgba => {
            ImageBuffer::<Rgba<u8>, _>::from_raw(output.width, output.height, pixels)
                .map(DynamicImage::ImageRgba8)
                .ok_or_else(|| anyhow!("failed to construct rgba image from decoded png buffer"))
        }
        PngColorType::Indexed => {
            Err(anyhow!("png decoder returned indexed output after normalization"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use anyhow::Result;
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
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

    fn tiny_grayscale_alpha_png_bytes() -> Vec<u8> {
        STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aF9sAAAAASUVORK5CYII=")
            .expect("decode tiny png fixture")
    }

    fn transparent_black_png_bytes() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([0, 0, 0, 0])));
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).expect("encode transparent png fixture");
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
            "gpt-5.4-mini",
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
        assert_eq!(output.model_name.as_deref(), Some("gpt-5.4-mini"));
        assert!(output.content_text.contains("diagram text"));
    }

    #[tokio::test]
    async fn normalizes_non_png_mime_payloads_before_provider_call() {
        let output = extract_image_with_provider(
            &FakeGateway,
            "openai",
            "gpt-5.4-mini",
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

    #[test]
    fn normalizes_tiny_grayscale_alpha_png_payloads() {
        let payload = prepare_vision_image_payload(&tiny_grayscale_alpha_png_bytes(), "image/png")
            .expect("normalize tiny grayscale alpha png");

        let decoded =
            image::load_from_memory(&payload.image_bytes).expect("decode normalized payload");

        assert_eq!(payload.mime_type, "image/png");
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
        assert_eq!(decoded.color(), image::ColorType::Rgb8);
        assert!(
            payload
                .warnings
                .iter()
                .any(|warning| warning.contains("upscaled image from 1x1 to 64x64"))
        );
    }

    #[test]
    fn transparent_images_are_matted_to_white_before_alpha_drop() {
        let payload = prepare_vision_image_payload(&transparent_black_png_bytes(), "image/png")
            .expect("normalize transparent png");

        let decoded = image::load_from_memory(&payload.image_bytes)
            .expect("decode transparent normalized payload")
            .to_rgb8();

        for pixel in decoded.pixels() {
            assert_eq!(pixel.0, [255, 255, 255]);
        }
    }
}
