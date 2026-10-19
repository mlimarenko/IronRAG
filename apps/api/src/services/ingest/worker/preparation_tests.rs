use super::preparation::{extract_artifact_text_and_checksum, vision_billing_usage_items};
use crate::services::content::service::preparation_fence_text;
use crate::shared::extraction::file_extract::build_inline_text_extraction_plan;
use sha2::{Digest, Sha256};

#[test]
fn worker_extract_artifact_matches_preparation_fence_without_normalized_text() {
    let source = "source-only preparation fence";
    let mut plan = build_inline_text_extraction_plan(source);
    plan.normalized_text = None;
    plan.source_text = Some(source.to_string());

    let (text, checksum) = extract_artifact_text_and_checksum(&plan);
    let preparation_text = preparation_fence_text(&plan);

    assert_eq!(text, source);
    assert_eq!(text, preparation_text);
    assert_eq!(checksum, hex::encode(Sha256::digest(preparation_text.as_bytes())));
}

#[test]
fn vision_billing_usage_items_expand_embedded_picture_calls() {
    let usage = serde_json::json!({
        "prompt_tokens": 30,
        "completion_tokens": 6,
        "embedded_picture_ocr_usage": [
            {"prompt_tokens": 10, "completion_tokens": 2},
            {"prompt_tokens": 20, "completion_tokens": 4}
        ]
    });

    let items = vision_billing_usage_items(&usage);

    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["prompt_tokens"], serde_json::json!(10));
    assert_eq!(items[1]["completion_tokens"], serde_json::json!(4));
}

#[test]
fn vision_billing_usage_items_keep_single_image_usage() {
    let usage = serde_json::json!({"prompt_tokens": 10, "completion_tokens": 2});

    let items = vision_billing_usage_items(&usage);

    assert_eq!(items, vec![usage]);
}
