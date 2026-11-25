use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::billing::CostCoverageStatus,
    infra::repositories::ingest_repository, interfaces::http::router_support::ApiError,
    services::ingest::service::ingest_stage_metadata,
};

mod billing;
mod stage_events;
#[cfg(test)]
mod tests;

use billing::{
    PipelineCallCounts, collect_document_level_stage_billing, load_pipeline_billing,
    merge_billing_into_stage_events,
};
use stage_events::{merge_stages, sort_attempt_entries_latest_first};

/// Document-wide cost fields.
///
/// Nothing measured stays absent: a coalesced `0` with an invented currency was
/// indistinguishable from a document whose provider calls were never priced.
/// `total_cost`/`currency_code` are therefore `Some` only when the ledger
/// priced at least one call, and `cost_coverage` describes the pricing
/// completeness of everything else that was recorded.
fn lifecycle_cost_fields(
    call_counts: PipelineCallCounts,
    charged_total: Decimal,
    charged_currency: Option<&str>,
) -> (Option<Decimal>, Option<String>, Option<CostCoverageStatus>) {
    let total_cost = call_counts.measured_cost(charged_total);
    (total_cost, total_cost.and(charged_currency.map(str::to_owned)), call_counts.coverage())
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLifecycleDetail {
    /// Cost measured by the provider-call ledger. `None` when no call in this
    /// document carries a catalog price — never a substituted zero.
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    /// Calls whose call state is `completed`. Usage and price may still be
    /// absent; see `unpricedProviderCallCount`.
    pub provider_call_count: Option<i64>,
    /// Calls whose liability no charge proves: completed calls without a price
    /// plus every reservation that never completed.
    pub unpriced_provider_call_count: Option<i64>,
    /// Reservations terminated as `failed` or `canceled`. The state proves no
    /// settled usage, not that the provider billed nothing.
    pub failed_provider_call_count: Option<i64>,
    /// Reservations still `started` when this document was read.
    pub unsettled_provider_call_count: Option<i64>,
    /// Pricing completeness of every call above. `None` when the ledger
    /// recorded no call for this document at all.
    pub cost_coverage: Option<CostCoverageStatus>,
    pub attempts: Vec<DocumentAttempt>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAttempt {
    pub job_id: Uuid,
    pub attempt_no: i32,
    pub attempt_kind: String,
    pub status: String,
    /// Stage the worker reported when the attempt ended or was updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    /// Durable typed failure class written by the ingest worker, e.g.
    /// `provider` or `worker_aborted`. `None` while the attempt has not failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    /// Stable machine-readable failure code for this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    /// Worker-produced failure message. Carries no provider response body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// Whether the bounded retry contract allows another attempt.
    pub retryable: bool,
    /// Cost attributed to this attempt's own provider calls; `None` when none of
    /// them carries a catalog price.
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub queue_started_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_elapsed_ms: Option<i64>,
    /// Calls whose call state is `completed`; a settled observation, not proof
    /// that a price was observed.
    pub provider_call_count: Option<i64>,
    /// Calls whose liability no charge proves, including reservations that never
    /// completed.
    pub unpriced_provider_call_count: Option<i64>,
    /// Reservations terminated as `failed` or `canceled`: no settled usage, and
    /// no proof the provider billed nothing.
    pub failed_provider_call_count: Option<i64>,
    pub unsettled_provider_call_count: Option<i64>,
    pub cost_coverage: Option<CostCoverageStatus>,
    pub stage_events: Vec<DocumentStageEvent>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentStageEvent {
    pub stage: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<i64>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    /// Cost measured for this stage by the provider-call ledger.
    pub estimated_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    /// Diff-aware ingest: number of chunks whose extraction output was reused
    /// from a previous revision because the chunk text was unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reused_chunks: Option<i64>,
    /// Number of entity contributions copied from the previous revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reused_entities: Option<i64>,
    /// Number of relation contributions copied from the previous revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reused_relations: Option<i64>,
    /// Total chunks the extraction stage processed (including reused ones).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks_processed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_call_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priced_provider_call_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpriced_provider_call_count: Option<i64>,
    /// Reservations terminated as `failed` or `canceled`: no settled usage, and
    /// no proof the provider billed nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_provider_call_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsettled_provider_call_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_coverage: Option<CostCoverageStatus>,
    /// Worker stage message (no provider response body).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

pub async fn load_document_lifecycle(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<DocumentLifecycleDetail, ApiError> {
    let jobs = ingest_repository::list_ingest_jobs_by_knowledge_document_id(
        &state.persistence.postgres,
        workspace_id,
        library_id,
        document_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "lifecycle: list jobs"))?;

    // Batch-load all attempts and stage events in two queries instead
    // of 2*N (one per job). The old N+1 loop was the dominant latency
    // contributor on documents with many retry attempts.
    let job_ids: Vec<Uuid> = jobs.iter().map(|j| j.id).collect();
    let all_attempt_rows =
        ingest_repository::list_ingest_attempts_by_jobs(&state.persistence.postgres, &job_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "lifecycle: list attempts"))?;
    let all_stage_rows =
        ingest_repository::list_ingest_stage_events_by_jobs(&state.persistence.postgres, &job_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "lifecycle: list stages"))?;

    let mut attempts_with_ids = Vec::new();
    for job in &jobs {
        let attempt_rows: Vec<&_> =
            all_attempt_rows.iter().filter(|a| a.job_id == job.id).collect();
        let stage_rows: Vec<&_> = all_stage_rows
            .iter()
            .filter(|s| attempt_rows.iter().any(|a| a.id == s.attempt_id))
            .collect();

        for ar in &attempt_rows {
            let my_stages: Vec<&ingest_repository::IngestStageEventRow> =
                stage_rows.iter().filter(|s| s.attempt_id == ar.id).copied().collect();
            let stage_events = merge_stages(&my_stages);
            let total_elapsed_ms =
                ar.finished_at.map(|fin| (fin - ar.started_at).num_milliseconds());

            attempts_with_ids.push((
                ar.id,
                DocumentAttempt {
                    job_id: job.id,
                    attempt_no: ar.attempt_number,
                    attempt_kind: job.job_kind.clone(),
                    status: ar.attempt_state.clone(),
                    current_stage: ar.current_stage.clone(),
                    failure_class: ar.failure_class.clone(),
                    failure_code: ar.failure_code.clone(),
                    failure_message: ar.failure_message.clone(),
                    retryable: ar.retryable,
                    total_cost: None,
                    currency_code: None,
                    queue_started_at: job.queued_at,
                    started_at: Some(ar.started_at),
                    finished_at: ar.finished_at,
                    total_elapsed_ms,
                    provider_call_count: None,
                    unpriced_provider_call_count: None,
                    failed_provider_call_count: None,
                    unsettled_provider_call_count: None,
                    cost_coverage: None,
                    stage_events,
                },
            ));
        }
    }
    sort_attempt_entries_latest_first(&mut attempts_with_ids);

    // Single pipeline billing query: provider calls → per-stage + total
    let billing = load_pipeline_billing(state, document_id).await?;

    // Stage events describe the worker lifecycle; billing calls are the source
    // of truth for model and cost attribution. Keep the attribution scoped to
    // the attempt that produced each provider call so retries do not inherit
    // stale model/cost rows from older attempts.
    let visible_attempt_ids =
        attempts_with_ids.iter().map(|(attempt_id, _)| *attempt_id).collect::<BTreeSet<_>>();

    for (attempt_id, attempt) in &mut attempts_with_ids {
        if let Some(stage_billing) = billing.per_stage_by_attempt.get(attempt_id) {
            merge_billing_into_stage_events(attempt, stage_billing);
        }
        if let Some(attempt_billing) = billing.total_by_attempt.get(attempt_id) {
            let counts = attempt_billing.call_counts;
            attempt.total_cost = counts.measured_cost(attempt_billing.cost);
            attempt.currency_code = attempt.total_cost.and(attempt_billing.currency.clone());
            attempt.provider_call_count = Some(counts.provider_call_count);
            attempt.unpriced_provider_call_count = Some(counts.unpriced_provider_call_count);
            attempt.failed_provider_call_count = Some(counts.failed_provider_call_count);
            attempt.unsettled_provider_call_count = Some(counts.unsettled_provider_call_count);
            attempt.cost_coverage = counts.coverage();
        }
    }

    // Provider calls that belong to no visible attempt (unattributed ledger
    // rows, or a hidden attempt) stay observable on the newest attempt. They
    // must not disappear just because no attempt has finished yet, and they are
    // never folded into another attempt's own total.
    let document_level_stage_billing =
        collect_document_level_stage_billing(&billing, &visible_attempt_ids);
    if !document_level_stage_billing.is_empty()
        && let Some((_, latest_attempt)) = attempts_with_ids.first_mut()
    {
        merge_billing_into_stage_events(latest_attempt, &document_level_stage_billing);
    }

    let (total_cost, currency_code, cost_coverage) =
        lifecycle_cost_fields(billing.call_counts, billing.total, billing.currency.as_deref());
    let attempts = attempts_with_ids.into_iter().map(|(_, attempt)| attempt).collect();

    Ok(DocumentLifecycleDetail {
        total_cost,
        currency_code,
        provider_call_count: Some(billing.call_counts.provider_call_count),
        unpriced_provider_call_count: Some(billing.call_counts.unpriced_provider_call_count),
        failed_provider_call_count: Some(billing.call_counts.failed_provider_call_count),
        unsettled_provider_call_count: Some(billing.call_counts.unsettled_provider_call_count),
        cost_coverage,
        attempts,
    })
}
