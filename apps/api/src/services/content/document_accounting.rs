use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::ingest_repository,
    interfaces::http::router_support::ApiError,
};

fn lifecycle_total_cost_fields(
    total: Decimal,
    currency: &str,
) -> (Option<Decimal>, Option<String>) {
    (Some(total), Some(currency.to_string()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLifecycleDetail {
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub attempts: Vec<DocumentAttempt>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAttempt {
    pub job_id: Uuid,
    pub attempt_no: i32,
    pub attempt_kind: String,
    pub status: String,
    pub queue_started_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_elapsed_ms: Option<i64>,
    pub stage_events: Vec<DocumentStageEvent>,
}

#[derive(Debug, Clone, Serialize)]
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

    let mut attempts = Vec::new();
    for job in &jobs {
        let attempt_rows =
            ingest_repository::list_ingest_attempts_by_job(&state.persistence.postgres, job.id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "lifecycle: list attempts"))?;
        let stage_rows =
            ingest_repository::list_ingest_stage_events_by_job(&state.persistence.postgres, job.id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "lifecycle: list stages"))?;

        for ar in &attempt_rows {
            let my_stages: Vec<&ingest_repository::IngestStageEventRow> =
                stage_rows.iter().filter(|s| s.attempt_id == ar.id).collect();
            let stage_events = merge_stages(&my_stages);
            let total_elapsed_ms =
                ar.finished_at.map(|fin| (fin - ar.started_at).num_milliseconds());

            attempts.push(DocumentAttempt {
                job_id: job.id,
                attempt_no: ar.attempt_number,
                attempt_kind: job.job_kind.clone(),
                status: ar.attempt_state.clone(),
                queue_started_at: job.queued_at,
                started_at: Some(ar.started_at),
                finished_at: ar.finished_at,
                total_elapsed_ms,
                stage_events,
            });
        }
    }
    attempts.sort_by(|a, b| b.queue_started_at.cmp(&a.queue_started_at));

    // Single canonical billing query: raw billing_charge → per-stage + total
    let billing = load_canonical_billing(state, document_id).await;

    // Apply per-stage costs to latest attempt
    if let Some(latest) = attempts.first_mut() {
        for (stage_name, cost, currency) in &billing.per_stage {
            if let Some(stage) = latest.stage_events.iter_mut().find(|s| s.stage == *stage_name) {
                stage.estimated_cost = Some(*cost);
                stage.currency_code = Some(currency.clone());
            }
        }
    }

    let (total_cost, currency_code) = lifecycle_total_cost_fields(billing.total, &billing.currency);

    Ok(DocumentLifecycleDetail { total_cost, currency_code, attempts })
}

fn merge_stages(rows: &[&ingest_repository::IngestStageEventRow]) -> Vec<DocumentStageEvent> {
    let mut out: Vec<DocumentStageEvent> = Vec::new();
    for row in rows {
        if let Some(ex) = out.iter_mut().find(|s| s.stage == row.stage_name) {
            if row.stage_state == "completed" || row.stage_state == "failed" {
                ex.status = row.stage_state.clone();
                ex.finished_at = Some(row.recorded_at);
                ex.elapsed_ms =
                    row.elapsed_ms.or(Some((row.recorded_at - ex.started_at).num_milliseconds()));
                ex.provider_kind = row
                    .provider_kind
                    .clone()
                    .or_else(|| {
                        row.details_json
                            .get("providerKind")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .or(ex.provider_kind.take());
                ex.model_name = row
                    .model_name
                    .clone()
                    .or_else(|| {
                        row.details_json.get("modelName").and_then(|v| v.as_str()).map(String::from)
                    })
                    .or(ex.model_name.take());
                ex.prompt_tokens = row.prompt_tokens.or(ex.prompt_tokens);
                ex.completion_tokens = row.completion_tokens.or(ex.completion_tokens);
                ex.total_tokens = row.total_tokens.or(ex.total_tokens);
                ex.estimated_cost = row.estimated_cost.or(ex.estimated_cost);
                ex.currency_code = row.currency_code.clone().or(ex.currency_code.take());
                ex.reused_chunks = row
                    .details_json
                    .get("reusedChunks")
                    .and_then(|v| v.as_i64())
                    .or(ex.reused_chunks);
                ex.reused_entities = row
                    .details_json
                    .get("reusedEntities")
                    .and_then(|v| v.as_i64())
                    .or(ex.reused_entities);
                ex.reused_relations = row
                    .details_json
                    .get("reusedRelations")
                    .and_then(|v| v.as_i64())
                    .or(ex.reused_relations);
                ex.chunks_processed = row
                    .details_json
                    .get("chunksProcessed")
                    .and_then(|v| v.as_i64())
                    .or(ex.chunks_processed);
            }
        } else {
            out.push(DocumentStageEvent {
                stage: row.stage_name.clone(),
                status: row.stage_state.clone(),
                started_at: row.recorded_at,
                finished_at: None,
                elapsed_ms: row.elapsed_ms,
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
                reused_chunks: row.details_json.get("reusedChunks").and_then(|v| v.as_i64()),
                reused_entities: row.details_json.get("reusedEntities").and_then(|v| v.as_i64()),
                reused_relations: row.details_json.get("reusedRelations").and_then(|v| v.as_i64()),
                chunks_processed: row.details_json.get("chunksProcessed").and_then(|v| v.as_i64()),
            });
        }
    }
    out
}

/// Single canonical billing query: raw `billing_charge` → per-stage costs + total.
/// This is the ONE source of truth for ALL cost displays.
struct CanonicalBilling {
    total: Decimal,
    currency: String,
    per_stage: Vec<(String, Decimal, String)>, // (stage_name, cost, currency)
}

async fn load_canonical_billing(state: &AppState, document_id: Uuid) -> CanonicalBilling {
    #[derive(sqlx::FromRow)]
    struct Row {
        call_kind: String,
        stage_cost: Decimal,
        currency_code: String,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT bpc.call_kind,
                SUM(bc.total_price) AS stage_cost,
                COALESCE(MAX(bc.currency_code), 'USD') AS currency_code
         FROM billing_charge bc
         JOIN billing_usage bu ON bu.id = bc.usage_id
         JOIN billing_provider_call bpc ON bpc.id = bu.provider_call_id
         LEFT JOIN ingest_attempt ia ON ia.id = bpc.owning_execution_id
           AND bpc.owning_execution_kind = 'ingest_attempt'
         LEFT JOIN ingest_job ij ON ij.id = ia.job_id
         LEFT JOIN runtime_graph_extraction rge ON rge.id = bpc.owning_execution_id
           AND bpc.owning_execution_kind = 'graph_extraction_attempt'
         WHERE ij.knowledge_document_id = $1 OR rge.document_id = $1
         GROUP BY bpc.call_kind",
    )
    .bind(document_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .unwrap_or_default();

    let mut total = Decimal::ZERO;
    let mut currency = "USD".to_string();
    let mut per_stage = Vec::new();

    for row in &rows {
        total += row.stage_cost;
        currency = row.currency_code.clone();

        // Map billing call_kind → pipeline stage name
        let stage_name = match row.call_kind.as_str() {
            "graph_extract" => "extract_graph",
            "embed_graph" | "embed_chunk" => "embed_chunk",
            "vision_extract" => "extract_content",
            "query_answer" | "query_rerank" => continue,
            other => other,
        };
        per_stage.push((stage_name.to_string(), row.stage_cost, row.currency_code.clone()));
    }

    CanonicalBilling { total, currency, per_stage }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::lifecycle_total_cost_fields;

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
}
