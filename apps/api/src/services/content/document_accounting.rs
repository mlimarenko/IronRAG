use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::ingest_repository,
    interfaces::http::router_support::ApiError,
    services::ingest::service::canonical_ingest_stage_metadata,
};

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

    // Single canonical billing query: provider calls → per-stage + total
    let billing = load_canonical_billing(state, document_id).await?;

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

fn merge_stages(rows: &[&ingest_repository::IngestStageEventRow]) -> Vec<DocumentStageEvent> {
    let mut out: Vec<DocumentStageEvent> = Vec::new();
    for row in rows {
        if let Some(ex) = out.iter_mut().find(|s| s.stage == row.stage_name) {
            merge_stage_details(&mut ex.details, &row.details_json);
            if row.stage_state == "completed" || row.stage_state == "failed" {
                if let Some(started_at) = row.started_at
                    && started_at < ex.started_at
                {
                    ex.started_at = started_at;
                }
                ex.status = row.stage_state.clone();
                ex.finished_at = Some(row.recorded_at);
                ex.elapsed_ms = row
                    .elapsed_ms
                    .or_else(|| Some((row.recorded_at - ex.started_at).num_milliseconds()));
                ex.provider_kind = row
                    .provider_kind
                    .clone()
                    .or_else(|| {
                        row.details_json
                            .get("providerKind")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .or_else(|| ex.provider_kind.take());
                ex.model_name = row
                    .model_name
                    .clone()
                    .or_else(|| {
                        row.details_json.get("modelName").and_then(|v| v.as_str()).map(String::from)
                    })
                    .or_else(|| ex.model_name.take());
                ex.prompt_tokens = row.prompt_tokens.or(ex.prompt_tokens);
                ex.completion_tokens = row.completion_tokens.or(ex.completion_tokens);
                ex.total_tokens = row.total_tokens.or(ex.total_tokens);
                ex.estimated_cost = row.estimated_cost.or(ex.estimated_cost);
                ex.currency_code = row.currency_code.clone().or_else(|| ex.currency_code.take());
                ex.reused_chunks = row
                    .details_json
                    .get("reusedChunks")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_chunks);
                ex.reused_entities = row
                    .details_json
                    .get("reusedEntities")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_entities);
                ex.reused_relations = row
                    .details_json
                    .get("reusedRelations")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_relations);
                ex.chunks_processed = row
                    .details_json
                    .get("chunksProcessed")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.chunks_processed);
            }
        } else {
            let started_at = row.started_at.unwrap_or(row.recorded_at);
            let is_terminal = row.stage_state == "completed" || row.stage_state == "failed";
            out.push(DocumentStageEvent {
                stage: row.stage_name.clone(),
                status: row.stage_state.clone(),
                started_at,
                finished_at: is_terminal.then_some(row.recorded_at),
                elapsed_ms: row.elapsed_ms.or_else(|| {
                    is_terminal.then_some((row.recorded_at - started_at).num_milliseconds())
                }),
                provider_kind: row.provider_kind.clone().or_else(|| {
                    row.details_json.get("providerKind").and_then(|v| v.as_str()).map(String::from)
                }),
                model_name: row.model_name.clone().or_else(|| {
                    row.details_json.get("modelName").and_then(|v| v.as_str()).map(String::from)
                }),
                prompt_tokens: row.prompt_tokens,
                completion_tokens: row.completion_tokens,
                total_tokens: row.total_tokens,
                estimated_cost: row.estimated_cost,
                currency_code: row.currency_code.clone(),
                reused_chunks: row
                    .details_json
                    .get("reusedChunks")
                    .and_then(serde_json::Value::as_i64),
                reused_entities: row
                    .details_json
                    .get("reusedEntities")
                    .and_then(serde_json::Value::as_i64),
                reused_relations: row
                    .details_json
                    .get("reusedRelations")
                    .and_then(serde_json::Value::as_i64),
                chunks_processed: row
                    .details_json
                    .get("chunksProcessed")
                    .and_then(serde_json::Value::as_i64),
                provider_call_count: None,
                details: stage_details_or_none(&row.details_json),
            });
        }
    }
    out
}

fn stage_details_or_none(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        _ => Some(value.clone()),
    }
}

fn merge_stage_details(target: &mut Option<Value>, incoming: &Value) {
    let Some(incoming) = stage_details_or_none(incoming) else {
        return;
    };

    match (target.as_mut(), incoming) {
        (Some(Value::Object(existing)), Value::Object(incoming)) => {
            for (key, value) in incoming {
                if !value.is_null() {
                    existing.insert(key, value);
                }
            }
        }
        (None, value) => {
            *target = Some(value);
        }
        (Some(_), value) => {
            *target = Some(value);
        }
    }
}

#[cfg(test)]
fn sort_attempts_latest_first(attempts: &mut [DocumentAttempt]) {
    attempts.sort_by_key(latest_attempt_sort_key);
}

fn sort_attempt_entries_latest_first(attempts: &mut [(Uuid, DocumentAttempt)]) {
    attempts.sort_by_key(|(_, attempt)| latest_attempt_sort_key(attempt));
}

fn latest_attempt_sort_key(
    attempt: &DocumentAttempt,
) -> (std::cmp::Reverse<DateTime<Utc>>, std::cmp::Reverse<i32>, std::cmp::Reverse<DateTime<Utc>>) {
    (
        std::cmp::Reverse(attempt.started_at.unwrap_or(attempt.queue_started_at)),
        std::cmp::Reverse(attempt.attempt_no),
        std::cmp::Reverse(attempt.queue_started_at),
    )
}

/// Single canonical billing query: provider calls + optional charges →
/// per-stage costs and call counts. This is the one source of truth for
/// document lifecycle model/cost observability.
#[derive(Debug)]
struct CanonicalBilling {
    total: Decimal,
    currency: String,
    total_by_attempt: BTreeMap<Uuid, CanonicalAttemptBilling>,
    per_stage_by_attempt: BTreeMap<Uuid, Vec<CanonicalStageBilling>>,
    unattributed_per_stage: Vec<CanonicalStageBilling>,
}

#[derive(Debug, Clone, PartialEq)]
struct CanonicalAttemptBilling {
    total: Decimal,
    currency: String,
}

#[derive(Debug, Clone, PartialEq)]
struct CanonicalStageBilling {
    stage_name: String,
    cost: Decimal,
    currency: String,
    provider_kind: Option<String>,
    model_name: Option<String>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    elapsed_ms: Option<i64>,
    provider_call_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CanonicalBillingRow {
    attempt_id: Option<Uuid>,
    call_kind: String,
    stage_cost: Decimal,
    currency_code: String,
    provider_kind: Option<String>,
    model_name: Option<String>,
    stage_started_at: Option<DateTime<Utc>>,
    stage_finished_at: Option<DateTime<Utc>>,
    provider_call_count: i64,
}

async fn load_canonical_billing(
    state: &AppState,
    document_id: Uuid,
) -> Result<CanonicalBilling, ApiError> {
    let rows = sqlx::query_as::<_, CanonicalBillingRow>(
        "WITH attributed_provider_call AS (
             SELECT
                COALESCE(ingest_attempt.id, graph_attempt.id) AS attempt_id,
                bpc.id,
                bpc.call_kind,
                bpc.provider_catalog_id,
                bpc.model_catalog_id,
                bpc.started_at,
                bpc.completed_at
         FROM billing_provider_call bpc
             LEFT JOIN billing_execution_cost execution_cost
                ON execution_cost.owning_execution_kind = bpc.owning_execution_kind
               AND execution_cost.owning_execution_id = bpc.owning_execution_id
               AND execution_cost.knowledge_document_id = $1
             LEFT JOIN ingest_attempt
                ON bpc.owning_execution_kind = 'ingest_attempt'
               AND ingest_attempt.id = bpc.owning_execution_id
             LEFT JOIN ingest_job
                ON ingest_job.id = ingest_attempt.job_id
             LEFT JOIN runtime_graph_extraction graph_extraction
                ON bpc.owning_execution_kind = 'graph_extraction_attempt'
               AND graph_extraction.id = bpc.owning_execution_id
             LEFT JOIN ingest_attempt graph_attempt
                ON graph_attempt.id::text =
                   graph_extraction.raw_output_json #>> '{lifecycle,activated_by_attempt_id}'
             WHERE ingest_job.knowledge_document_id = $1
                OR graph_extraction.document_id = $1
                OR execution_cost.knowledge_document_id = $1
         )
         SELECT attributed_provider_call.attempt_id,
                attributed_provider_call.call_kind,
                COALESCE(SUM(bc.total_price), 0) AS stage_cost,
                COALESCE(bc.currency_code, 'USD') AS currency_code,
                NULLIF(string_agg(DISTINCT apc.provider_kind, ', ' ORDER BY apc.provider_kind), '') AS provider_kind,
                NULLIF(string_agg(DISTINCT amc.model_name, ', ' ORDER BY amc.model_name), '') AS model_name,
                MIN(attributed_provider_call.started_at) AS stage_started_at,
                MAX(attributed_provider_call.completed_at) AS stage_finished_at,
                COUNT(DISTINCT attributed_provider_call.id)::bigint AS provider_call_count
         FROM attributed_provider_call
         LEFT JOIN billing_usage bu ON bu.provider_call_id = attributed_provider_call.id
         LEFT JOIN billing_charge bc ON bc.usage_id = bu.id
         JOIN ai_provider_catalog apc ON apc.id = attributed_provider_call.provider_catalog_id
         JOIN ai_model_catalog amc ON amc.id = attributed_provider_call.model_catalog_id
         GROUP BY
             attributed_provider_call.attempt_id,
             attributed_provider_call.call_kind,
             COALESCE(bc.currency_code, 'USD')",
    )
    .bind(document_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .map_err(|e| ApiError::internal_with_log(e, "lifecycle: load canonical billing"))?;

    canonical_billing_from_rows(&rows)
}

fn canonical_billing_from_rows(rows: &[CanonicalBillingRow]) -> Result<CanonicalBilling, ApiError> {
    let mut nonzero_currency: Option<&str> = None;
    for row in rows.iter().filter(|row| row.stage_cost != Decimal::ZERO) {
        match nonzero_currency {
            None => nonzero_currency = Some(&row.currency_code),
            Some(currency_code) if currency_code == row.currency_code.as_str() => {}
            Some(_) => {
                return Err(ApiError::Conflict(
                    "document lifecycle has billing charges in multiple currencies; request per-currency charges"
                        .to_string(),
                ));
            }
        }
    }
    let currency = nonzero_currency.unwrap_or("USD").to_string();
    let mut total = Decimal::ZERO;
    let mut total_by_attempt: BTreeMap<Uuid, CanonicalAttemptBilling> = BTreeMap::new();
    let mut per_stage_by_attempt: BTreeMap<Uuid, BTreeMap<String, CanonicalStageBilling>> =
        BTreeMap::new();
    let mut unattributed_per_stage_by_name: BTreeMap<String, CanonicalStageBilling> =
        BTreeMap::new();

    for row in rows {
        total += row.stage_cost;

        let Some(stage_name) = billing_stage_name(&row.call_kind) else {
            continue;
        };
        let Some(attempt_id) = row.attempt_id else {
            let entry = unattributed_per_stage_by_name
                .entry(stage_name.to_string())
                .or_insert_with(|| CanonicalStageBilling {
                    stage_name: stage_name.to_string(),
                    cost: Decimal::ZERO,
                    currency: currency.clone(),
                    provider_kind: None,
                    model_name: None,
                    started_at: None,
                    finished_at: None,
                    elapsed_ms: None,
                    provider_call_count: 0,
                });
            entry.cost += row.stage_cost;
            entry.currency = currency.clone();
            merge_distinct_csv(&mut entry.provider_kind, row.provider_kind.as_deref());
            merge_distinct_csv(&mut entry.model_name, row.model_name.as_deref());
            merge_stage_bounds(entry, row.stage_started_at, row.stage_finished_at);
            entry.provider_call_count += row.provider_call_count;
            continue;
        };
        let attempt_total = total_by_attempt.entry(attempt_id).or_insert_with(|| {
            CanonicalAttemptBilling { total: Decimal::ZERO, currency: currency.clone() }
        });
        attempt_total.total += row.stage_cost;
        attempt_total.currency = currency.clone();

        let entry = per_stage_by_attempt
            .entry(attempt_id)
            .or_default()
            .entry(stage_name.to_string())
            .or_insert_with(|| CanonicalStageBilling {
                stage_name: stage_name.to_string(),
                cost: Decimal::ZERO,
                currency: currency.clone(),
                provider_kind: None,
                model_name: None,
                started_at: None,
                finished_at: None,
                elapsed_ms: None,
                provider_call_count: 0,
            });
        entry.cost += row.stage_cost;
        entry.currency = currency.clone();
        merge_distinct_csv(&mut entry.provider_kind, row.provider_kind.as_deref());
        merge_distinct_csv(&mut entry.model_name, row.model_name.as_deref());
        merge_stage_bounds(entry, row.stage_started_at, row.stage_finished_at);
        entry.provider_call_count += row.provider_call_count;
    }

    let per_stage_by_attempt = per_stage_by_attempt
        .into_iter()
        .map(|(attempt_id, stages)| {
            let stages = stages
                .into_values()
                .map(|mut stage| {
                    stage.elapsed_ms = match (stage.started_at.as_ref(), stage.finished_at.as_ref())
                    {
                        (Some(started_at), Some(finished_at)) => {
                            Some((*finished_at - *started_at).num_milliseconds())
                        }
                        _ => None,
                    };
                    stage
                })
                .collect();
            (attempt_id, stages)
        })
        .collect();
    let unattributed_per_stage = finalize_stage_billing(unattributed_per_stage_by_name);
    Ok(CanonicalBilling {
        total,
        currency,
        total_by_attempt,
        per_stage_by_attempt,
        unattributed_per_stage,
    })
}

fn finalize_stage_billing(
    stages: BTreeMap<String, CanonicalStageBilling>,
) -> Vec<CanonicalStageBilling> {
    stages
        .into_values()
        .map(|mut stage| {
            stage.elapsed_ms = match (stage.started_at.as_ref(), stage.finished_at.as_ref()) {
                (Some(started_at), Some(finished_at)) => {
                    Some((*finished_at - *started_at).num_milliseconds())
                }
                _ => None,
            };
            stage
        })
        .collect()
}

fn collect_document_level_stage_billing(
    billing: &CanonicalBilling,
    visible_attempt_ids: &BTreeSet<Uuid>,
) -> Vec<CanonicalStageBilling> {
    let mut document_level_by_stage = BTreeMap::new();
    merge_stage_billing_group(&mut document_level_by_stage, &billing.unattributed_per_stage);

    for (attempt_id, stages) in &billing.per_stage_by_attempt {
        if !visible_attempt_ids.contains(attempt_id) {
            merge_stage_billing_group(&mut document_level_by_stage, stages);
        }
    }

    finalize_stage_billing(document_level_by_stage)
}

fn merge_stage_billing_group(
    target: &mut BTreeMap<String, CanonicalStageBilling>,
    incoming: &[CanonicalStageBilling],
) {
    for billing in incoming {
        let entry =
            target.entry(billing.stage_name.clone()).or_insert_with(|| CanonicalStageBilling {
                stage_name: billing.stage_name.clone(),
                cost: Decimal::ZERO,
                currency: billing.currency.clone(),
                provider_kind: None,
                model_name: None,
                started_at: None,
                finished_at: None,
                elapsed_ms: None,
                provider_call_count: 0,
            });
        entry.cost += billing.cost;
        entry.currency = billing.currency.clone();
        merge_distinct_csv(&mut entry.provider_kind, billing.provider_kind.as_deref());
        merge_distinct_csv(&mut entry.model_name, billing.model_name.as_deref());
        merge_stage_bounds(entry, billing.started_at, billing.finished_at);
        entry.provider_call_count += billing.provider_call_count;
    }
}

fn merge_stage_bounds(
    stage: &mut CanonicalStageBilling,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
) {
    if let Some(started_at) = started_at
        && stage.started_at.as_ref().is_none_or(|existing| started_at < *existing)
    {
        stage.started_at = Some(started_at);
    }
    if let Some(finished_at) = finished_at
        && stage.finished_at.as_ref().is_none_or(|existing| finished_at > *existing)
    {
        stage.finished_at = Some(finished_at);
    }
}

fn merge_billing_into_stage_events(
    attempt: &mut DocumentAttempt,
    stage_billing: &[CanonicalStageBilling],
) {
    let fallback_started_at = attempt.started_at.unwrap_or(attempt.queue_started_at);

    for billing in stage_billing {
        if let Some(stage) = attempt.stage_events.iter_mut().find(|s| s.stage == billing.stage_name)
        {
            stage.estimated_cost = Some(billing.cost);
            stage.currency_code = Some(billing.currency.clone());
            if stage.model_name.as_deref().unwrap_or("").is_empty() {
                stage.model_name = billing.model_name.clone();
            }
            if stage.provider_kind.as_deref().unwrap_or("").is_empty() {
                stage.provider_kind = billing.provider_kind.clone();
            }
            if stage.finished_at.is_none() {
                stage.finished_at = billing.finished_at;
            }
            if stage.elapsed_ms.is_none() {
                stage.elapsed_ms = billing.elapsed_ms;
            }
            stage.provider_call_count = Some(billing.provider_call_count);
            continue;
        }

        attempt.stage_events.push(DocumentStageEvent {
            stage: billing.stage_name.clone(),
            status: "completed".to_string(),
            started_at: billing.started_at.unwrap_or(fallback_started_at),
            finished_at: billing.finished_at,
            elapsed_ms: billing.elapsed_ms,
            provider_kind: billing.provider_kind.clone(),
            model_name: billing.model_name.clone(),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            estimated_cost: Some(billing.cost),
            currency_code: Some(billing.currency.clone()),
            reused_chunks: None,
            reused_entities: None,
            reused_relations: None,
            chunks_processed: None,
            provider_call_count: Some(billing.provider_call_count),
            details: None,
        });
    }

    attempt.stage_events.sort_by(|left, right| {
        stage_order(&left.stage)
            .cmp(&stage_order(&right.stage))
            .then_with(|| left.started_at.cmp(&right.started_at))
            .then_with(|| left.stage.cmp(&right.stage))
    });
}

fn stage_order(stage: &str) -> u8 {
    canonical_ingest_stage_metadata(stage)
        .map_or(u8::MAX, |metadata| u8::try_from(metadata.stage_rank).unwrap_or(u8::MAX))
}

fn billing_stage_name(call_kind: &str) -> Option<&str> {
    match call_kind {
        "graph_extract" => Some("extract_graph"),
        "embed_chunk" => Some("embed_chunk"),
        "vision_extract" => Some("extract_content"),
        "query_answer" | "query_rerank" => None,
        other => Some(other),
    }
}

fn merge_distinct_csv(target: &mut Option<String>, incoming: Option<&str>) {
    let mut values = target
        .as_deref()
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if let Some(incoming) = incoming {
        for value in incoming.split(',').map(str::trim).filter(|value| !value.is_empty()) {
            if !values.iter().any(|existing| existing == value) {
                values.push(value.to_string());
            }
        }
    }

    if !values.is_empty() {
        *target = Some(values.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use serde_json::json;
    use uuid::Uuid;

    use crate::infra::repositories::ingest_repository;

    use super::{
        CanonicalBillingRow, CanonicalStageBilling, DocumentAttempt, DocumentStageEvent,
        canonical_billing_from_rows, collect_document_level_stage_billing,
        lifecycle_total_cost_fields, merge_billing_into_stage_events, merge_stages,
        sort_attempts_latest_first,
    };

    #[test]
    fn lifecycle_cost_fields_keep_zero_cost_visible() {
        let (total_cost, currency_code) = lifecycle_total_cost_fields(Decimal::ZERO, "USD");

        assert_eq!(total_cost, Some(Decimal::ZERO));
        assert_eq!(currency_code.as_deref(), Some("USD"));
    }

    #[test]
    fn lifecycle_cost_fields_keep_non_zero_cost_visible() {
        let amount = Decimal::from_str_exact("0.1234").expect("valid decimal");
        let (total_cost, currency_code) = lifecycle_total_cost_fields(amount, "USD");

        assert_eq!(total_cost, Some(amount));
        assert_eq!(currency_code.as_deref(), Some("USD"));
    }

    #[test]
    fn canonical_billing_merges_call_kinds_that_share_pipeline_stage() {
        let attempt_id = Uuid::now_v7();
        let rows = vec![
            CanonicalBillingRow {
                attempt_id: Some(attempt_id),
                call_kind: "graph_extract".to_string(),
                stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-chat-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
                provider_call_count: 2,
            },
            CanonicalBillingRow {
                attempt_id: Some(attempt_id),
                call_kind: "graph_extract".to_string(),
                stage_cost: Decimal::from_str_exact("0.2500").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-beta".to_string()),
                model_name: Some("alpha-embedding-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:03Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:07Z")),
                provider_call_count: 5,
            },
        ];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");
        let per_stage =
            billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

        assert_eq!(billing.total, Decimal::from_str_exact("0.3500").expect("valid decimal"));
        assert_eq!(
            billing.total_by_attempt[&attempt_id].total,
            Decimal::from_str_exact("0.3500").expect("valid decimal")
        );
        assert_eq!(per_stage.len(), 1);
        assert_eq!(per_stage[0].stage_name, "extract_graph");
        assert_eq!(per_stage[0].cost, Decimal::from_str_exact("0.3500").expect("valid decimal"));
        assert_eq!(per_stage[0].provider_kind.as_deref(), Some("provider-alpha, provider-beta"));
        assert_eq!(
            per_stage[0].model_name.as_deref(),
            Some("alpha-chat-large, alpha-embedding-large")
        );
        assert_eq!(per_stage[0].elapsed_ms, Some(7000));
        assert_eq!(per_stage[0].provider_call_count, 7);
    }

    #[test]
    fn canonical_billing_rejects_mixed_currencies_before_summing() {
        let attempt_id = Uuid::now_v7();
        let usd = CanonicalBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
            provider_call_count: 1,
        };
        let mut eur = usd.clone();
        eur.stage_cost = Decimal::from_str_exact("0.2000").expect("valid decimal");
        eur.currency_code = "EUR".to_string();

        let error = canonical_billing_from_rows(&[usd, eur])
            .expect_err("unlike currencies must never be combined into one document total");

        assert!(error.to_string().contains("multiple currencies"));
    }

    #[test]
    fn canonical_billing_keeps_zero_cost_provider_calls_visible() {
        let attempt_id = Uuid::now_v7();
        let rows = vec![CanonicalBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: Decimal::ZERO,
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:01:00Z")),
            provider_call_count: 3,
        }];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");
        let per_stage =
            billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

        assert_eq!(billing.total, Decimal::ZERO);
        assert_eq!(billing.total_by_attempt[&attempt_id].total, Decimal::ZERO);
        assert_eq!(per_stage.len(), 1);
        assert_eq!(per_stage[0].stage_name, "embed_chunk");
        assert_eq!(per_stage[0].cost, Decimal::ZERO);
        assert_eq!(per_stage[0].model_name.as_deref(), Some("alpha-embedding-large"));
        assert_eq!(per_stage[0].elapsed_ms, Some(60000));
        assert_eq!(per_stage[0].provider_call_count, 3);
    }

    #[test]
    fn document_level_stage_billing_keeps_hidden_attempt_costs_observable() {
        let visible_attempt_id = Uuid::now_v7();
        let hidden_attempt_id = Uuid::now_v7();
        let rows = vec![
            CanonicalBillingRow {
                attempt_id: Some(visible_attempt_id),
                call_kind: "graph_extract".to_string(),
                stage_cost: Decimal::from_str_exact("0.0100").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-chat-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
                provider_call_count: 1,
            },
            CanonicalBillingRow {
                attempt_id: Some(hidden_attempt_id),
                call_kind: "embed_chunk".to_string(),
                stage_cost: Decimal::from_str_exact("0.1200").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-beta".to_string()),
                model_name: Some("beta-embedding-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:02Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:04Z")),
                provider_call_count: 1,
            },
            CanonicalBillingRow {
                attempt_id: Some(hidden_attempt_id),
                call_kind: "graph_extract".to_string(),
                stage_cost: Decimal::from_str_exact("0.3400").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-chat-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:05Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:09Z")),
                provider_call_count: 4,
            },
        ];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");
        let visible_attempt_ids = BTreeSet::from([visible_attempt_id]);
        let document_level_stages =
            collect_document_level_stage_billing(&billing, &visible_attempt_ids);

        assert_eq!(
            document_level_stages.iter().map(|stage| stage.stage_name.as_str()).collect::<Vec<_>>(),
            vec!["embed_chunk", "extract_graph"]
        );
        assert_eq!(
            document_level_stages[0].cost,
            Decimal::from_str_exact("0.1200").expect("valid decimal")
        );
        assert_eq!(
            document_level_stages[1].cost,
            Decimal::from_str_exact("0.3400").expect("valid decimal")
        );
        assert_eq!(document_level_stages[1].provider_call_count, 4);
    }

    #[test]
    fn canonical_billing_ignores_query_call_kinds_for_document_pipeline_stages() {
        let rows = vec![CanonicalBillingRow {
            attempt_id: Some(Uuid::now_v7()),
            call_kind: "query_answer".to_string(),
            stage_cost: Decimal::from_str_exact("1.5000").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
            provider_call_count: 1,
        }];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");

        assert_eq!(billing.total, Decimal::from_str_exact("1.5000").expect("valid decimal"));
        assert!(billing.total_by_attempt.is_empty());
        assert!(billing.per_stage_by_attempt.is_empty());
    }

    #[test]
    fn canonical_billing_keeps_document_stage_cost_when_attempt_link_is_absent() {
        let rows = vec![CanonicalBillingRow {
            attempt_id: None,
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.2500").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:05Z")),
            provider_call_count: 4,
        }];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");

        assert_eq!(billing.total, Decimal::from_str_exact("0.2500").expect("valid decimal"));
        assert!(billing.total_by_attempt.is_empty());
        assert!(billing.per_stage_by_attempt.is_empty());
        assert_eq!(billing.unattributed_per_stage.len(), 1);
        assert_eq!(billing.unattributed_per_stage[0].stage_name, "extract_graph");
        assert_eq!(billing.unattributed_per_stage[0].elapsed_ms, Some(5000));
        assert_eq!(billing.unattributed_per_stage[0].provider_call_count, 4);
    }

    #[test]
    fn canonical_billing_keeps_retry_attempt_stage_costs_isolated() {
        let first_attempt_id = Uuid::now_v7();
        let retry_attempt_id = Uuid::now_v7();
        let rows = vec![
            CanonicalBillingRow {
                attempt_id: Some(first_attempt_id),
                call_kind: "embed_chunk".to_string(),
                stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-embedding-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
                stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
                provider_call_count: 1,
            },
            CanonicalBillingRow {
                attempt_id: Some(retry_attempt_id),
                call_kind: "embed_chunk".to_string(),
                stage_cost: Decimal::from_str_exact("0.2000").expect("valid decimal"),
                currency_code: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-embedding-large".to_string()),
                stage_started_at: Some(utc("2026-05-12T10:05:00Z")),
                stage_finished_at: Some(utc("2026-05-12T10:05:03Z")),
                provider_call_count: 2,
            },
        ];

        let billing = canonical_billing_from_rows(&rows)
            .expect("single-currency billing rows should aggregate");

        assert_eq!(billing.total, Decimal::from_str_exact("0.3000").expect("valid decimal"));
        assert_eq!(
            billing.per_stage_by_attempt[&first_attempt_id][0].cost,
            Decimal::from_str_exact("0.1000").expect("valid decimal")
        );
        assert_eq!(billing.per_stage_by_attempt[&first_attempt_id][0].provider_call_count, 1);
        assert_eq!(
            billing.per_stage_by_attempt[&retry_attempt_id][0].cost,
            Decimal::from_str_exact("0.2000").expect("valid decimal")
        );
        assert_eq!(billing.per_stage_by_attempt[&retry_attempt_id][0].provider_call_count, 2);
    }

    #[test]
    fn billing_materializes_missing_pipeline_stages_on_latest_attempt() {
        let mut attempt = DocumentAttempt {
            job_id: Uuid::now_v7(),
            attempt_no: 1,
            attempt_kind: "document_upload".to_string(),
            status: "succeeded".to_string(),
            total_cost: None,
            currency_code: None,
            queue_started_at: utc("2026-05-12T10:00:00Z"),
            started_at: Some(utc("2026-05-12T10:00:00Z")),
            finished_at: Some(utc("2026-05-12T10:00:30Z")),
            total_elapsed_ms: Some(30000),
            stage_events: vec![DocumentStageEvent {
                stage: "extract_content".to_string(),
                status: "completed".to_string(),
                started_at: utc("2026-05-12T10:00:00Z"),
                finished_at: Some(utc("2026-05-12T10:00:02Z")),
                elapsed_ms: Some(2000),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                estimated_cost: None,
                currency_code: None,
                reused_chunks: None,
                reused_entities: None,
                reused_relations: None,
                chunks_processed: None,
                provider_call_count: None,
                details: None,
            }],
        };
        let stage_billing = vec![
            CanonicalStageBilling {
                stage_name: "extract_graph".to_string(),
                cost: Decimal::from_str_exact("0.4500").expect("valid decimal"),
                currency: "USD".to_string(),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-chat-large".to_string()),
                started_at: Some(utc("2026-05-12T10:00:20Z")),
                finished_at: Some(utc("2026-05-12T10:00:29Z")),
                elapsed_ms: Some(9000),
                provider_call_count: 9,
            },
            CanonicalStageBilling {
                stage_name: "embed_chunk".to_string(),
                cost: Decimal::from_str_exact("0.1200").expect("valid decimal"),
                currency: "USD".to_string(),
                provider_kind: Some("provider-beta".to_string()),
                model_name: Some("beta-embedding-large".to_string()),
                started_at: Some(utc("2026-05-12T10:00:10Z")),
                finished_at: Some(utc("2026-05-12T10:00:13Z")),
                elapsed_ms: Some(3000),
                provider_call_count: 3,
            },
        ];

        merge_billing_into_stage_events(&mut attempt, &stage_billing);

        assert_eq!(
            attempt.stage_events.iter().map(|stage| stage.stage.as_str()).collect::<Vec<_>>(),
            vec!["extract_content", "embed_chunk", "extract_graph"]
        );
        let embed_stage = attempt
            .stage_events
            .iter()
            .find(|stage| stage.stage == "embed_chunk")
            .expect("materialized embed stage");
        assert_eq!(embed_stage.status, "completed");
        assert_eq!(embed_stage.elapsed_ms, Some(3000));
        assert_eq!(embed_stage.model_name.as_deref(), Some("beta-embedding-large"));
        assert_eq!(embed_stage.provider_kind.as_deref(), Some("provider-beta"));
        assert_eq!(embed_stage.provider_call_count, Some(3));
        assert_eq!(
            embed_stage.estimated_cost,
            Some(Decimal::from_str_exact("0.1200").expect("valid decimal"))
        );
    }

    #[test]
    fn terminal_stage_event_keeps_started_finished_and_elapsed_time() {
        let row = ingest_repository::IngestStageEventRow {
            id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            stage_name: "extract_content".to_string(),
            stage_state: "completed".to_string(),
            ordinal: 10,
            message: None,
            details_json: json!({}),
            recorded_at: utc("2026-05-12T10:00:12Z"),
            provider_kind: None,
            model_name: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_tokens: None,
            estimated_cost: None,
            currency_code: None,
            elapsed_ms: None,
            started_at: Some(utc("2026-05-12T10:00:02Z")),
        };

        let stages = merge_stages(&[&row]);

        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].stage, "extract_content");
        assert_eq!(stages[0].status, "completed");
        assert_eq!(stages[0].started_at, utc("2026-05-12T10:00:02Z"));
        assert_eq!(stages[0].finished_at, Some(utc("2026-05-12T10:00:12Z")));
        assert_eq!(stages[0].elapsed_ms, Some(10000));
    }

    #[test]
    fn attempts_with_same_queue_time_sort_by_latest_retry_first() {
        let queue_started_at = utc("2026-05-12T10:00:00Z");
        let mut attempts = vec![
            attempt_summary(1, "failed", queue_started_at, "2026-05-12T10:00:01Z"),
            attempt_summary(4, "succeeded", queue_started_at, "2026-05-12T10:30:00Z"),
            attempt_summary(2, "failed", queue_started_at, "2026-05-12T10:05:00Z"),
        ];

        sort_attempts_latest_first(&mut attempts);

        assert_eq!(attempts[0].attempt_no, 4);
        assert_eq!(attempts[0].status, "succeeded");
        assert_eq!(attempts[1].attempt_no, 2);
        assert_eq!(attempts[2].attempt_no, 1);
    }

    fn attempt_summary(
        attempt_no: i32,
        status: &str,
        queue_started_at: DateTime<Utc>,
        started_at: &str,
    ) -> DocumentAttempt {
        DocumentAttempt {
            job_id: Uuid::now_v7(),
            attempt_no,
            attempt_kind: "document_upload".to_string(),
            status: status.to_string(),
            total_cost: None,
            currency_code: None,
            queue_started_at,
            started_at: Some(utc(started_at)),
            finished_at: None,
            total_elapsed_ms: None,
            stage_events: Vec::new(),
        }
    }

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).expect("valid timestamp").with_timezone(&Utc)
    }
}
