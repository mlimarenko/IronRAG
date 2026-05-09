use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::integrations::retry::{
    ProviderCallError, RetryPolicy, provider_http_status_error, with_retry,
};

const JINA_RERANK_URL: &str = "https://api.jina.ai/v1/rerank";
const JINA_RERANK_TIMEOUT_MS: u64 = 1200;

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct JinaRerankRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: Vec<&'a str>,
    top_n: usize,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct JinaRerankResponse {
    results: Vec<JinaRerankResult>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct JinaRerankResult {
    index: usize,
    relevance_score: f32,
}

/// Calls Jina reranker API and returns (original_index, relevance_score) pairs sorted
/// by descending relevance score.
pub async fn jina_rerank(
    api_key: &str,
    query: &str,
    documents: &[String],
    top_n: usize,
) -> Result<Vec<(usize, f32)>> {
    let client = Client::builder()
        .timeout(Duration::from_millis(JINA_RERANK_TIMEOUT_MS))
        .build()
        .context("failed to build HTTP client for jina reranker")?;

    let body = JinaRerankRequest {
        model: "jina-reranker-v2-base-multilingual",
        query,
        documents: documents.iter().map(String::as_str).collect(),
        top_n,
    };

    let parsed: JinaRerankResponse = with_retry(
        || async {
            let response = crate::observability::inject_trace_context(
                client
                    .post(JINA_RERANK_URL)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {api_key}")),
            )
            .json(&body)
            .send()
            .await
            .map_err(|source| {
                ProviderCallError::transport("jina reranker request failed", source)
            })?;

            let status = response.status();
            if status.is_success() {
                return response.json().await.map_err(|source| {
                    ProviderCallError::response_json(
                        "failed to parse jina reranker response",
                        source,
                    )
                });
            }

            let headers = response.headers().clone();
            let body_text = response.text().await.unwrap_or_default();
            Err(provider_http_status_error("jina", status, &headers, &body_text))
        },
        RetryPolicy::default(),
    )
    .await?;

    let mut ranked: Vec<(usize, f32)> =
        parsed.results.into_iter().map(|r| (r.index, r.relevance_score)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    Ok(ranked)
}
