use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::ingest_repository,
    interfaces::http::router_support::ApiError, services::ingest::service::ingest_stage_metadata,
};

mod billing;
mod stage_events;
#[cfg(test)]
mod tests;

use billing::{
    collect_document_level_stage_billing, load_pipeline_billing, merge_billing_into_stage_events,
};
use stage_events::{merge_stages, sort_attempt_entries_latest_first};

fn lifecycle_total_cost_fields(
    total: Decimal,
    currency: &str,
) -> (Option<Decimal>, Option<String>) {
    (Some(total), Some(currency.to_string()))
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLifecycleDetail {
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub attempts: Vec<DocumentAttempt>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAttempt {
    pub job_id: Uuid,
    pub attempt_no: i32,
    pub attempt_kind: String,
    pub status: String,
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub queue_started_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_elapsed_ms: Option<i64>,
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
                    total_cost: None,
                    currency_code: None,
                    queue_started_at: job.queued_at,
                    started_at: Some(ar.started_at),
                    finished_at: ar.finished_at,
                    total_elapsed_ms,
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
            attempt.total_cost = Some(attempt_billing.total);
            attempt.currency_code = Some(attempt_billing.currency.clone());
        }
    }

    let document_level_stage_billing =
        collect_document_level_stage_billing(&billing, &visible_attempt_ids);
    if !document_level_stage_billing.is_empty()
        && let Some((_, terminal_attempt)) =
            attempts_with_ids.iter_mut().find(|(_, attempt)| attempt.finished_at.is_some())
    {
        merge_billing_into_stage_events(terminal_attempt, &document_level_stage_billing);
        terminal_attempt.total_cost = Some(billing.total);
        terminal_attempt.currency_code = Some(billing.currency.clone());
    }

    let (total_cost, currency_code) = lifecycle_total_cost_fields(billing.total, &billing.currency);
    let attempts = attempts_with_ids.into_iter().map(|(_, attempt)| attempt).collect();

    Ok(DocumentLifecycleDetail { total_cost, currency_code, attempts })
}
