//! Shared helper for decoding axum test responses in integration suites.

use anyhow::{Context, Result};
use http_body_util::BodyExt;
use serde_json::Value;

/// Collects a response body and decodes it as JSON, returning `Null` for an
/// empty body and including the raw payload in the error when decoding fails.
pub(crate) async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).with_context(|| {
        format!("failed to decode response json: {}", String::from_utf8_lossy(&bytes))
    })
}
