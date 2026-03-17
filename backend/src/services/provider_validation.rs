use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::{
    app::state::AppState,
    integrations::llm::{ChatRequest, EmbeddingRequest, VisionRequest},
};

pub async fn validate_chat_provider(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> Result<()> {
    state
        .llm_gateway
        .generate(ChatRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: "Reply with the single word ok.".to_string(),
        })
        .await?;
    Ok(())
}

pub async fn validate_embedding_provider(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> Result<()> {
    state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            input: "provider validation".to_string(),
        })
        .await?;
    Ok(())
}

pub async fn validate_vision_provider(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> Result<()> {
    // A small valid PNG fixture used only to prove the provider can parse image input.
    const VALIDATION_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAMAAAAoLQ9TAAAABGdBTUEAALGPC/xhBQAAAAFzUkdCAK7OHOkAAAClUExURUdwTAFAYAMAAAkJEADE/wHG/wG6/wCr8wBQigQVIwDI/wC1/wDG/wGBuQDE/wDC/wLF/3JmZgDA/wC//wHB/wHA/8bDwwTK/wEAAQYfMgCKxgCm7AJpmAIBCuvr7B4kLQIpQP///2VgZBcbJJyXmQAFEAJikAJYg4WGi729wPjz8QIxSw0KESpAUADO/wJJbC8vNgB4rQKf4nR0eQBci6OlqeHk5aSkMKIAAAAwdFJOUwD9+/mYk/r9A/402Tf+oKes+7u2t7z+Ov/////////////+/////////////////hBoBGMAAADNSURBVBjTPY/HkoMwEEQHGyRkoJx3FUHkDI77/5+2klzlufU79LwGMBf7EcaRHwOENoaBkCNjoxT+J1842XgP7G0Iv1oS8O1E5ZLMSm25D7AT5Kdr0Hob+r4n4heC1zHvmobenu8hP7YHiMYir+o6K2mdvZ8MAWa0+1NlmmZaeJRhC4phtoA+pAXRuN7ZRMusniRaNYKg8RZ8bzXtqnZZTal5m/AEM1P94kTETkypuc8rOjkxCM9GvRjSwqifvuM0Y9qNcwR2e4Qx2rv5/6XoFfDFvPX9AAAAAElFTkSuQmCC";
    let image_bytes = BASE64_STANDARD.decode(VALIDATION_PNG_BASE64)?;

    state
        .llm_gateway
        .vision_extract(VisionRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            prompt: "Reply with the single word ok.".to_string(),
            image_bytes,
            mime_type: "image/png".to_string(),
        })
        .await?;
    Ok(())
}
