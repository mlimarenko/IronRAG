use super::*;

/// Single pipeline billing query: provider calls + optional charges →
/// per-stage costs and call counts. This is the one source of truth for
/// document lifecycle model/cost observability.
#[derive(Debug)]
pub(super) struct PipelineBilling {
    pub(super) total: Decimal,
    pub(super) currency: String,
    pub(super) total_by_attempt: BTreeMap<Uuid, PipelineAttemptBilling>,
    pub(super) per_stage_by_attempt: BTreeMap<Uuid, Vec<PipelineStageBilling>>,
    pub(super) unattributed_per_stage: Vec<PipelineStageBilling>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PipelineAttemptBilling {
    pub(super) total: Decimal,
    pub(super) currency: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PipelineStageBilling {
    pub(super) stage_name: String,
    pub(super) cost: Decimal,
    pub(super) currency: String,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) started_at: Option<DateTime<Utc>>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) elapsed_ms: Option<i64>,
    pub(super) provider_call_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct PipelineBillingRow {
    pub(super) attempt_id: Option<Uuid>,
    pub(super) call_kind: String,
    pub(super) stage_cost: Decimal,
    pub(super) currency_code: String,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) stage_started_at: Option<DateTime<Utc>>,
    pub(super) stage_finished_at: Option<DateTime<Utc>>,
    pub(super) provider_call_count: i64,
}

pub(super) async fn load_pipeline_billing(
    state: &AppState,
    document_id: Uuid,
) -> Result<PipelineBilling, ApiError> {
    let rows = sqlx::query_as::<_, PipelineBillingRow>(
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
    .map_err(|e| ApiError::internal_with_log(e, "lifecycle: load pipeline billing"))?;

    aggregate_pipeline_billing(&rows)
}

pub(super) fn aggregate_pipeline_billing(
    rows: &[PipelineBillingRow],
) -> Result<PipelineBilling, ApiError> {
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
    let mut total_by_attempt: BTreeMap<Uuid, PipelineAttemptBilling> = BTreeMap::new();
    let mut per_stage_by_attempt: BTreeMap<Uuid, BTreeMap<String, PipelineStageBilling>> =
        BTreeMap::new();
    let mut unattributed_per_stage_by_name: BTreeMap<String, PipelineStageBilling> =
        BTreeMap::new();

    for row in rows {
        total += row.stage_cost;

        let Some(stage_name) = billing_stage_name(&row.call_kind) else {
            continue;
        };
        let Some(attempt_id) = row.attempt_id else {
            let entry = unattributed_per_stage_by_name
                .entry(stage_name.to_string())
                .or_insert_with(|| PipelineStageBilling {
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
            PipelineAttemptBilling { total: Decimal::ZERO, currency: currency.clone() }
        });
        attempt_total.total += row.stage_cost;
        attempt_total.currency = currency.clone();

        let entry = per_stage_by_attempt
            .entry(attempt_id)
            .or_default()
            .entry(stage_name.to_string())
            .or_insert_with(|| PipelineStageBilling {
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
    Ok(PipelineBilling {
        total,
        currency,
        total_by_attempt,
        per_stage_by_attempt,
        unattributed_per_stage,
    })
}

fn finalize_stage_billing(
    stages: BTreeMap<String, PipelineStageBilling>,
) -> Vec<PipelineStageBilling> {
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

pub(super) fn collect_document_level_stage_billing(
    billing: &PipelineBilling,
    visible_attempt_ids: &BTreeSet<Uuid>,
) -> Vec<PipelineStageBilling> {
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
    target: &mut BTreeMap<String, PipelineStageBilling>,
    incoming: &[PipelineStageBilling],
) {
    for billing in incoming {
        let entry =
            target.entry(billing.stage_name.clone()).or_insert_with(|| PipelineStageBilling {
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
    stage: &mut PipelineStageBilling,
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

pub(super) fn merge_billing_into_stage_events(
    attempt: &mut DocumentAttempt,
    stage_billing: &[PipelineStageBilling],
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
    ingest_stage_metadata(stage)
        .map_or(u8::MAX, |metadata| u8::try_from(metadata.stage_rank).unwrap_or(u8::MAX))
}

fn billing_stage_name(call_kind: &str) -> Option<&str> {
    match call_kind {
        "graph_extract" | "graph_entity_type" => Some("extract_graph"),
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
