use super::*;

use crate::domains::billing::CostCoverageStatus;

/// Provider-call accounting for one document, attempt, or pipeline stage.
///
/// A provider call exists as a durable reservation taken before the paid
/// request (`started`) and is settled afterwards with provider-reported usage.
/// A call that never reached `completed` proves no settled usage — it may still
/// have been dispatched and billed — so it is excluded from the settled count,
/// still counted as unpriced, and never lets the scope read as a fully measured
/// zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PipelineCallCounts {
    /// Calls whose call state is `completed`. A completed call may still report
    /// no usage or no price; those are counted as unpriced below.
    pub(super) provider_call_count: i64,
    /// Completed calls that recorded at least one charge.
    pub(super) priced_provider_call_count: i64,
    /// Calls whose liability no charge proves: completed calls without a price
    /// plus every call that never completed.
    pub(super) unpriced_provider_call_count: i64,
    /// Reservations terminated as `failed` or `canceled`. The state proves no
    /// settled usage, not that nothing was dispatched or billed: a failure can
    /// happen after the request left.
    pub(super) failed_provider_call_count: i64,
    /// Reservations still `started` when the scope was read; their liability is
    /// unknown until they settle.
    pub(super) unsettled_provider_call_count: i64,
}

impl PipelineCallCounts {
    fn from_row(row: &PipelineBillingRow) -> Self {
        Self {
            provider_call_count: row.provider_call_count,
            priced_provider_call_count: row.priced_provider_call_count,
            unpriced_provider_call_count: row.total_provider_call_count
                - row.priced_provider_call_count,
            failed_provider_call_count: row.failed_provider_call_count,
            unsettled_provider_call_count: row.unsettled_provider_call_count,
        }
    }

    /// No provider call was recorded for this scope at all.
    pub(super) const fn is_empty(&self) -> bool {
        self.provider_call_count == 0 && self.unpriced_provider_call_count == 0
    }

    fn merge(&mut self, other: Self) {
        self.provider_call_count += other.provider_call_count;
        self.priced_provider_call_count += other.priced_provider_call_count;
        self.unpriced_provider_call_count += other.unpriced_provider_call_count;
        self.failed_provider_call_count += other.failed_provider_call_count;
        self.unsettled_provider_call_count += other.unsettled_provider_call_count;
    }

    /// Cost measured by the provider-call ledger, or `None` when nothing in
    /// scope carries a price.
    ///
    /// A scope with no recorded provider call is not zero either: the ledger
    /// never observed that there was nothing to pay for, so an absent
    /// measurement stays absent. `Some(Decimal::ZERO)` is only returned for
    /// calls the price catalog actually priced at zero.
    pub(super) fn measured_cost(&self, charged: Decimal) -> Option<Decimal> {
        if self.priced_provider_call_count > 0 { Some(charged) } else { None }
    }

    /// Pricing completeness of the scope, or `None` when the ledger recorded no
    /// provider call for it. Claiming `complete` there would assert a measured
    /// zero this ledger never observed.
    ///
    /// The two counts are disjoint observations, so the denominator is their
    /// sum: a priced call plus an unresolved reservation is `partial`, not
    /// `unknown`. Passing the completed count as the denominator mixed the two.
    pub(super) fn coverage(&self) -> Option<CostCoverageStatus> {
        if self.is_empty() {
            return None;
        }
        Some(CostCoverageStatus::from_call_counts(
            self.priced_provider_call_count + self.unpriced_provider_call_count,
            self.unpriced_provider_call_count,
        ))
    }

    /// Status of a stage the ledger materializes because no stage event row
    /// exists for it.
    ///
    /// Provider-call records never prove that the worker finished the stage: an
    /// open reservation keeps it `running`, and a failed or canceled attempt is
    /// not evidence of completion, even when another call in the same stage
    /// completed. Only the worker's own stage event proves completion, and an
    /// existing stage event keeps its own status.
    pub(super) fn stage_status(&self) -> &'static str {
        if self.unsettled_provider_call_count > 0 {
            "running"
        } else if self.failed_provider_call_count > 0 {
            "failed"
        } else if self.provider_call_count > 0 {
            "completed"
        } else {
            "running"
        }
    }
}

/// Single pipeline billing query result: provider calls + optional charges →
/// per-stage costs and call counts. This is the one source of truth for
/// document lifecycle model/cost observability.
#[derive(Debug)]
pub(super) struct PipelineBilling {
    /// Sum of ledger charges across the whole document.
    pub(super) total: Decimal,
    /// Currency of the priced calls, absent when the document has no charge.
    pub(super) currency: Option<String>,
    pub(super) call_counts: PipelineCallCounts,
    pub(super) total_by_attempt: BTreeMap<Uuid, PipelineAttemptBilling>,
    pub(super) per_stage_by_attempt: BTreeMap<Uuid, Vec<PipelineStageBilling>>,
    pub(super) unattributed_per_stage: Vec<PipelineStageBilling>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PipelineAttemptBilling {
    pub(super) cost: Decimal,
    pub(super) currency: Option<String>,
    pub(super) call_counts: PipelineCallCounts,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PipelineStageBilling {
    pub(super) stage_name: String,
    pub(super) cost: Decimal,
    pub(super) currency: Option<String>,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) started_at: Option<DateTime<Utc>>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) elapsed_ms: Option<i64>,
    pub(super) call_counts: PipelineCallCounts,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub(super) struct PipelineBillingRow {
    pub(super) attempt_id: Option<Uuid>,
    pub(super) call_kind: String,
    pub(super) stage_cost: Decimal,
    /// Currency of this group's charges; absent when every call in the group
    /// is unpriced. Never defaulted to a currency the ledger did not read.
    pub(super) currency_code: Option<String>,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) stage_started_at: Option<DateTime<Utc>>,
    pub(super) stage_finished_at: Option<DateTime<Utc>>,
    pub(super) total_provider_call_count: i64,
    pub(super) provider_call_count: i64,
    pub(super) priced_provider_call_count: i64,
    pub(super) failed_provider_call_count: i64,
    pub(super) unsettled_provider_call_count: i64,
}

pub(super) async fn load_pipeline_billing(
    state: &AppState,
    document_id: Uuid,
) -> Result<PipelineBilling, ApiError> {
    // One row per provider call: the charge aggregate is a lateral so a call
    // with several usage rows cannot fan the cost or the call count out. The
    // call state is carried through so settled calls stay distinguishable from
    // reservations that never completed.
    let rows = sqlx::query_as::<_, PipelineBillingRow>(
        "WITH attributed_provider_call AS (
             SELECT
                COALESCE(ingest_attempt.id, graph_attempt.id) AS attempt_id,
                bpc.id,
                bpc.call_kind,
                bpc.call_state::text AS call_state,
                apc.provider_kind,
                amc.model_name,
                bpc.started_at,
                bpc.completed_at,
                charges.charged_currency AS currency_code,
                COALESCE(charges.charged_total, 0) AS charged_total,
                COALESCE(charges.charge_count, 0) AS charge_count
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
                 JOIN ai_provider_catalog apc ON apc.id = bpc.provider_catalog_id
                 JOIN ai_model_catalog amc ON amc.id = bpc.model_catalog_id
                 LEFT JOIN LATERAL (
                     SELECT
                        min(charge.currency_code) AS charged_currency,
                        sum(charge.total_price) AS charged_total,
                        count(*)::bigint AS charge_count
                     FROM billing_usage usage
                     JOIN billing_charge charge ON charge.usage_id = usage.id
                     WHERE usage.provider_call_id = bpc.id
                 ) charges ON true
             WHERE ingest_job.knowledge_document_id = $1
                OR graph_extraction.document_id = $1
                OR execution_cost.knowledge_document_id = $1
         )
         SELECT attributed_provider_call.attempt_id,
                attributed_provider_call.call_kind,
                attributed_provider_call.currency_code,
                COALESCE(SUM(attributed_provider_call.charged_total), 0) AS stage_cost,
                NULLIF(string_agg(DISTINCT attributed_provider_call.provider_kind, ', ' ORDER BY attributed_provider_call.provider_kind), '') AS provider_kind,
                NULLIF(string_agg(DISTINCT attributed_provider_call.model_name, ', ' ORDER BY attributed_provider_call.model_name), '') AS model_name,
                MIN(attributed_provider_call.started_at) AS stage_started_at,
                MAX(attributed_provider_call.completed_at) AS stage_finished_at,
                COUNT(*)::bigint AS total_provider_call_count,
                COUNT(*) FILTER (
                    WHERE attributed_provider_call.call_state = 'completed'
                )::bigint AS provider_call_count,
                COUNT(*) FILTER (
                    WHERE attributed_provider_call.call_state = 'completed'
                      AND attributed_provider_call.charge_count > 0
                )::bigint AS priced_provider_call_count,
                COUNT(*) FILTER (
                    WHERE attributed_provider_call.call_state IN ('failed', 'canceled')
                )::bigint AS failed_provider_call_count,
                COUNT(*) FILTER (
                    WHERE attributed_provider_call.call_state = 'started'
                )::bigint AS unsettled_provider_call_count
         FROM attributed_provider_call
         GROUP BY
             attributed_provider_call.attempt_id,
             attributed_provider_call.call_kind,
             attributed_provider_call.currency_code",
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
    // A currency is only reported from a group that actually charged
    // something. A document whose calls are all unpriced keeps a `None`
    // currency instead of inventing one.
    let mut currency: Option<String> = None;
    for row in rows.iter().filter(|row| row.priced_provider_call_count > 0) {
        let Some(row_currency) = row.currency_code.as_deref() else {
            continue;
        };
        match currency.as_deref() {
            None => currency = Some(row_currency.to_string()),
            Some(existing) if existing == row_currency => {}
            Some(_) => {
                return Err(ApiError::Conflict(
                    "document lifecycle has billing charges in multiple currencies; request per-currency charges"
                        .to_string(),
                ));
            }
        }
    }

    let mut total = Decimal::ZERO;
    let mut call_counts = PipelineCallCounts::default();
    let mut total_by_attempt: BTreeMap<Uuid, PipelineAttemptBilling> = BTreeMap::new();
    let mut per_stage_by_attempt: BTreeMap<Uuid, BTreeMap<String, PipelineStageBilling>> =
        BTreeMap::new();
    let mut unattributed_per_stage_by_name: BTreeMap<String, PipelineStageBilling> =
        BTreeMap::new();

    for row in rows {
        total += row.stage_cost;
        let row_counts = PipelineCallCounts::from_row(row);
        call_counts.merge(row_counts);

        let Some(stage_name) = billing_stage_name(&row.call_kind) else {
            continue;
        };
        let Some(attempt_id) = row.attempt_id else {
            let entry = unattributed_per_stage_by_name
                .entry(stage_name.to_string())
                .or_insert_with(|| empty_stage_billing(stage_name));
            merge_stage_row(entry, row, row_counts);
            continue;
        };
        let attempt_total =
            total_by_attempt.entry(attempt_id).or_insert_with(|| PipelineAttemptBilling {
                cost: Decimal::ZERO,
                currency: None,
                call_counts: PipelineCallCounts::default(),
            });
        attempt_total.cost += row.stage_cost;
        attempt_total.call_counts.merge(row_counts);
        if row_counts.priced_provider_call_count > 0 {
            attempt_total.currency = row.currency_code.clone();
        }

        let entry = per_stage_by_attempt
            .entry(attempt_id)
            .or_default()
            .entry(stage_name.to_string())
            .or_insert_with(|| empty_stage_billing(stage_name));
        merge_stage_row(entry, row, row_counts);
    }

    let per_stage_by_attempt = per_stage_by_attempt
        .into_iter()
        .map(|(attempt_id, stages)| (attempt_id, finalize_stage_billing(stages)))
        .collect();
    let unattributed_per_stage = finalize_stage_billing(unattributed_per_stage_by_name);
    Ok(PipelineBilling {
        total,
        currency,
        call_counts,
        total_by_attempt,
        per_stage_by_attempt,
        unattributed_per_stage,
    })
}

fn empty_stage_billing(stage_name: &str) -> PipelineStageBilling {
    PipelineStageBilling {
        stage_name: stage_name.to_string(),
        cost: Decimal::ZERO,
        currency: None,
        provider_kind: None,
        model_name: None,
        started_at: None,
        finished_at: None,
        elapsed_ms: None,
        call_counts: PipelineCallCounts::default(),
    }
}

fn merge_stage_row(
    entry: &mut PipelineStageBilling,
    row: &PipelineBillingRow,
    row_counts: PipelineCallCounts,
) {
    entry.cost += row.stage_cost;
    entry.call_counts.merge(row_counts);
    if row_counts.priced_provider_call_count > 0 {
        entry.currency = row.currency_code.clone();
    }
    merge_distinct_csv(&mut entry.provider_kind, row.provider_kind.as_deref());
    merge_distinct_csv(&mut entry.model_name, row.model_name.as_deref());
    merge_stage_bounds(entry, row.stage_started_at, row.stage_finished_at);
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
        let entry = target
            .entry(billing.stage_name.clone())
            .or_insert_with(|| empty_stage_billing(&billing.stage_name));
        entry.cost += billing.cost;
        entry.call_counts.merge(billing.call_counts);
        if billing.call_counts.priced_provider_call_count > 0 {
            entry.currency = billing.currency.clone();
        }
        merge_distinct_csv(&mut entry.provider_kind, billing.provider_kind.as_deref());
        merge_distinct_csv(&mut entry.model_name, billing.model_name.as_deref());
        merge_stage_bounds(entry, billing.started_at, billing.finished_at);
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
        let measured_cost = billing.call_counts.measured_cost(billing.cost);
        let coverage = billing.call_counts.coverage();

        if let Some(stage) = attempt.stage_events.iter_mut().find(|s| s.stage == billing.stage_name)
        {
            // The worker's own stage measurement wins over a ledger that has
            // nothing priced to say; an unpriced ledger must not overwrite it
            // with a coalesced zero.
            if measured_cost.is_some() {
                stage.estimated_cost = measured_cost;
                stage.currency_code = billing.currency.clone();
            }
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
            stage.provider_call_count = Some(billing.call_counts.provider_call_count);
            stage.priced_provider_call_count = Some(billing.call_counts.priced_provider_call_count);
            stage.unpriced_provider_call_count =
                Some(billing.call_counts.unpriced_provider_call_count);
            stage.failed_provider_call_count = Some(billing.call_counts.failed_provider_call_count);
            stage.unsettled_provider_call_count =
                Some(billing.call_counts.unsettled_provider_call_count);
            stage.cost_coverage = coverage;
            continue;
        }

        attempt.stage_events.push(DocumentStageEvent {
            stage: billing.stage_name.clone(),
            status: billing.call_counts.stage_status().to_string(),
            started_at: billing.started_at.unwrap_or(fallback_started_at),
            finished_at: billing.finished_at,
            elapsed_ms: billing.elapsed_ms,
            provider_kind: billing.provider_kind.clone(),
            model_name: billing.model_name.clone(),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            estimated_cost: measured_cost,
            currency_code: billing.currency.clone(),
            reused_chunks: None,
            reused_entities: None,
            reused_relations: None,
            chunks_processed: None,
            provider_call_count: Some(billing.call_counts.provider_call_count),
            priced_provider_call_count: Some(billing.call_counts.priced_provider_call_count),
            unpriced_provider_call_count: Some(billing.call_counts.unpriced_provider_call_count),
            failed_provider_call_count: Some(billing.call_counts.failed_provider_call_count),
            unsettled_provider_call_count: Some(billing.call_counts.unsettled_provider_call_count),
            cost_coverage: coverage,
            message: None,
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
