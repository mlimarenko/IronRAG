use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::{
    integrations::llm::{LlmGateway, build_structured_chat_request},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        graph::extract::prompt::graph_extraction_response_format,
        graph::extract::types::{
            GraphExtractionCallTiming, GraphExtractionLifecycle, GraphExtractionPromptPlan,
            RawGraphExtractionResponse,
        },
        graph::extract::{GraphExtractionActivity, graph_extraction_prompt_hash},
        ingest::cancellation::{StageError, ensure_not_cancelled},
    },
};

use super::usage_json::build_provider_usage_json;

pub(crate) async fn request_graph_extraction_with_prompt_plan(
    gateway: &dyn LlmGateway,
    runtime_binding: &ResolvedRuntimeBinding,
    prompt_plan: &GraphExtractionPromptPlan,
    lifecycle: GraphExtractionLifecycle,
    cancellation_token: &CancellationToken,
) -> Result<RawGraphExtractionResponse> {
    ensure_not_cancelled(cancellation_token)?;
    let prompt_hash = graph_extraction_prompt_hash(&prompt_plan.prompt, runtime_binding);
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let started_at = Utc::now();
    let started = Instant::now();
    let request = build_structured_chat_request(
        runtime_binding.chat_request_seed(),
        prompt_plan.prompt.clone(),
        graph_extraction_response_format(),
    );
    // The gateway owns both provider admission and the HTTP request. Keep the
    // revision watchdog suspended for that whole operation; the transport's
    // own deadline remains responsible for turning a hung call into a typed,
    // retryable provider timeout.
    let _provider_operation = GraphExtractionActivity::begin_current_provider_operation();
    let response = tokio::select! {
        () = cancellation_token.cancelled() => {
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = gateway.generate(request) => result.context("graph extraction provider call failed")?,
    };
    ensure_not_cancelled(cancellation_token)?;
    let finished_at = Utc::now();
    let output_text = response.output_text;
    let usage_json = build_provider_usage_json(&provider_kind, &model_name, response.usage_json);

    Ok(RawGraphExtractionResponse {
        provider_kind,
        model_name,
        prompt_hash,
        request_shape_key: prompt_plan.request_shape_key.clone(),
        request_size_bytes: prompt_plan.request_size_bytes,
        output_text: output_text.clone(),
        usage_json: usage_json.clone(),
        lifecycle,
        timing: build_graph_extraction_call_timing(
            started_at,
            finished_at,
            started.elapsed(),
            &prompt_plan.prompt,
            &output_text,
            &usage_json,
        ),
    })
}

fn build_graph_extraction_call_timing(
    started_at: chrono::DateTime<Utc>,
    finished_at: chrono::DateTime<Utc>,
    elapsed: std::time::Duration,
    prompt: &str,
    output_text: &str,
    usage_json: &serde_json::Value,
) -> GraphExtractionCallTiming {
    let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let input_char_count = i32::try_from(prompt.chars().count()).unwrap_or(i32::MAX);
    let output_char_count = i32::try_from(output_text.chars().count()).unwrap_or(i32::MAX);
    let total_tokens =
        usage_json.get("total_tokens").and_then(serde_json::Value::as_i64).or_else(|| {
            let prompt_tokens =
                usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)?;
            let completion_tokens = usage_json
                .get("completion_tokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Some(prompt_tokens.saturating_add(completion_tokens))
        });
    let seconds = (elapsed_ms > 0).then_some(elapsed_ms as f64 / 1000.0);

    GraphExtractionCallTiming {
        started_at,
        finished_at,
        elapsed_ms,
        input_char_count,
        output_char_count,
        chars_per_second: seconds.and_then(|value| {
            (value > 0.0)
                .then_some(f64::from(input_char_count.saturating_add(output_char_count)) / value)
        }),
        tokens_per_second: seconds.and_then(|value| {
            total_tokens.filter(|tokens| *tokens > 0).map(|tokens| tokens as f64 / value)
        }),
    }
}
