use anyhow::Result;

use crate::{
    integrations::llm::{LlmGateway, VisionRequest},
    shared::extraction::ExtractionOutput,
};

pub async fn extract_image_with_provider(
    gateway: &dyn LlmGateway,
    provider_kind: &str,
    model_name: &str,
    mime_type: &str,
    file_bytes: &[u8],
) -> Result<ExtractionOutput> {
    let response = gateway
        .vision_extract(VisionRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: "Return only the text visible in this image as plain text. Do not add headings, explanations, summaries, entity lists, markdown, or quotes. If no readable text is visible, return an empty string."
                .to_string(),
            image_bytes: file_bytes.to_vec(),
            mime_type: mime_type.to_string(),
        })
        .await?;

    Ok(ExtractionOutput {
        extraction_kind: "vision_image".into(),
        content_text: response.output_text,
        page_count: Some(1),
        warnings: Vec::new(),
        source_map: serde_json::json!({
            "mime_type": mime_type,
        }),
        provider_kind: Some(response.provider_kind),
        model_name: Some(response.model_name),
    })
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionResponse,
    };

    struct FakeGateway;

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
                output_text: "diagram text and entities".to_string(),
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
            "image/png",
            &[0x89, 0x50, 0x4E, 0x47],
        )
        .await
        .expect("image extraction");

        assert_eq!(output.extraction_kind, "vision_image");
        assert_eq!(output.page_count, Some(1));
        assert_eq!(output.provider_kind.as_deref(), Some("openai"));
        assert_eq!(output.model_name.as_deref(), Some("gpt-5-mini"));
        assert!(output.content_text.contains("diagram text"));
    }
}
