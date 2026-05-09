use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::integrations::retry::{
    ProviderCallError, RetryPolicy, provider_http_status_error, with_retry,
};

const COHERE_RERANK_URL: &str = "https://api.cohere.com/v2/rerank";
const COHERE_RERANK_TIMEOUT_MS: u64 = 1200;

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct CohereRerankRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: Vec<&'a str>,
    top_n: usize,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct CohereRerankResponse {
    results: Vec<CohereRerankResult>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct CohereRerankResult {
    index: usize,
    relevance_score: f32,
}

/// Calls Cohere reranker API and returns (original_index, relevance_score) pairs sorted
/// by descending relevance score.
pub async fn cohere_rerank(
    api_key: &str,
    query: &str,
    documents: &[String],
    top_n: usize,
) -> Result<Vec<(usize, f32)>> {
    let client = Client::builder()
        .timeout(Duration::from_millis(COHERE_RERANK_TIMEOUT_MS))
        .build()
        .context("failed to build HTTP client for cohere reranker")?;

    let body = CohereRerankRequest {
        model: "rerank-v3.5",
        query,
        documents: documents.iter().map(String::as_str).collect(),
        top_n,
    };

    let parsed: CohereRerankResponse = with_retry(
        || async {
            let response = crate::observability::inject_trace_context(
                client
                    .post(COHERE_RERANK_URL)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {api_key}")),
            )
            .json(&body)
            .send()
            .await
            .map_err(|source| {
                ProviderCallError::transport("cohere reranker request failed", source)
            })?;

            let status = response.status();
            if status.is_success() {
                return response.json().await.map_err(|source| {
                    ProviderCallError::response_json(
                        "failed to parse cohere reranker response",
                        source,
                    )
                });
            }

            let headers = response.headers().clone();
            let body_text = response.text().await.unwrap_or_default();
            Err(provider_http_status_error("cohere", status, &headers, &body_text))
        },
        RetryPolicy::default(),
    )
    .await?;

    let mut ranked: Vec<(usize, f32)> =
        parsed.results.into_iter().map(|r| (r.index, r.relevance_score)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    Ok(ranked)
}
