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
fn vision_billing_uses_raw_calls_not_the_aggregate_and_retains_empty_attempts() {
    let usage = serde_json::json!({
        "total_tokens":30,
        "vision_calls":[
            {"provider_kind":"openai","model_name":"vision","usage_json":{"total_tokens":10}},
            {"provider_kind":"openai","model_name":"vision","usage_json":{"total_tokens":20}},
            {"provider_kind":"openai","model_name":"vision","usage_json":{}},
        ],
    });
    let items = vision_billing_usage_items(&usage).unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].usage_json["total_tokens"], 10);
    assert_eq!(items[1].usage_json["total_tokens"], 20);
    assert_eq!(items[2].usage_json, serde_json::json!({}));
    assert_eq!(items[0].provider_kind, "openai");
}

#[test]
fn native_only_does_not_fabricate_provider_usage_and_malformed_receipts_fail_closed() {
    assert!(vision_billing_usage_items(&serde_json::json!({})).unwrap().is_empty());
    assert!(
        vision_billing_usage_items(&serde_json::json!({"vision_calls":[{"usage_json":{}}]}))
            .is_err()
    );
}
