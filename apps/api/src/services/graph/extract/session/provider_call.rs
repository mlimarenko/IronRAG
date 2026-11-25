use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    integrations::llm::{ChatRequest, ChatResponse, LlmGateway, build_structured_chat_request},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        graph::extract::prompt::graph_extraction_response_format,
        graph::extract::types::{
            GraphExtractionCallTiming, GraphExtractionLifecycle, GraphExtractionPromptPlan,
            RawGraphExtractionResponse,
        },
        graph::extract::{GraphExtractionActivity, graph_extraction_prompt_hash},
        ingest::cancellation::{StageError, ensure_not_cancelled},
        ops::billing::ReserveExecutionProviderCallCommand,
        ops::billing::provider_reservation::ProviderCallReservation,
    },
};

use super::usage_json::build_provider_usage_json;

/// Billing and cancellation surface of one graph provider call.
pub(crate) struct GraphExtractionProviderCall<'a> {
    pub(crate) lifecycle: GraphExtractionLifecycle,
    pub(crate) billing:
        Option<&'a (&'a crate::app::state::AppState, ReserveExecutionProviderCallCommand)>,
    pub(crate) reserved_call_id: &'a mut Option<Uuid>,
    pub(crate) cancellation_token: &'a CancellationToken,
}

pub(crate) async fn request_graph_extraction_with_prompt_plan(
    gateway: &dyn LlmGateway,
    runtime_binding: &ResolvedRuntimeBinding,
    prompt_plan: &GraphExtractionPromptPlan,
    provider_call: GraphExtractionProviderCall<'_>,
) -> Result<RawGraphExtractionResponse> {
    let GraphExtractionProviderCall { lifecycle, billing, reserved_call_id, cancellation_token } =
        provider_call;
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
    // The reservation carries the attribution of this logical call and the row
    // of its reply; every physical send creates and owns its own durable row.
    let mut reservation = reserve_graph_extraction_provider_call(billing, reserved_call_id).await?;
    ensure_not_cancelled(cancellation_token)?;
    let call = run_graph_extraction_provider_call(gateway, request, &reservation);
    let call_result = tokio::select! {
        () = cancellation_token.cancelled() => {
            // Cancellation terminalizes through the reservation's Drop as
            // `canceled`; returning before `terminalize_graph_extraction_failure`
            // keeps the base audit label instead of overwriting it with `failed`.
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = call => result.map_err(|error| error.context("graph extraction provider call failed")),
    };
    let response = match call_result {
        Ok(response) => response,
        Err(error) => {
            return Err(terminalize_graph_extraction_failure(&mut reservation, error).await);
        }
    };
    let finished_at = Utc::now();
    let output_text = response.output_text;
    let usage_json = build_provider_usage_json(&provider_kind, &model_name, response.usage_json);
    settle_graph_extraction_usage(&mut reservation, &usage_json).await;
    ensure_not_cancelled(cancellation_token)?;

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

/// Binds the logical call to its attribution; no row is written yet.
async fn reserve_graph_extraction_provider_call(
    billing: Option<&(&crate::app::state::AppState, ReserveExecutionProviderCallCommand)>,
    reserved_call_id: &mut Option<Uuid>,
) -> Result<Option<ProviderCallReservation>> {
    match billing {
        Some((state, command)) => {
            let reservation = ProviderCallReservation::reserve_command(state, command.clone())
                .await
                .context("graph provider billing reservation failed")?;
            *reserved_call_id = Some(reservation.provider_call_id());
            Ok(Some(reservation))
        }
        None => Ok(None),
    }
}

/// Runs the gateway call under the reservation scope.
async fn run_graph_extraction_provider_call(
    gateway: &dyn LlmGateway,
    request: ChatRequest,
    reservation: &Option<ProviderCallReservation>,
) -> Result<ChatResponse> {
    match reservation {
        Some(reservation) => reservation.scoped(gateway.generate(request)).await,
        None => gateway.generate(request).await,
    }
}

/// Terminalizes a failed reservation and keeps the original call error.
async fn terminalize_graph_extraction_failure(
    reservation: &mut Option<ProviderCallReservation>,
    error: anyhow::Error,
) -> anyhow::Error {
    if let Some(reservation) = reservation
        && let Err(settlement_error) = reservation.fail().await
    {
        tracing::error!(
            provider_call_id = %reservation.provider_call_id(),
            error = %settlement_error,
            "graph reservation terminalization failed; reconciliation scheduled"
        );
    }
    error
}

/// Settles the paid response's usage without discarding it.
async fn settle_graph_extraction_usage(
    reservation: &mut Option<ProviderCallReservation>,
    usage_json: &serde_json::Value,
) {
    if let Some(reservation) = reservation
        && let Err(error) = reservation.complete(usage_json).await
    {
        tracing::error!(
            provider_call_id = %reservation.provider_call_id(),
            %error,
            "graph usage settlement failed; reconciliation scheduled"
        );
    }
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
