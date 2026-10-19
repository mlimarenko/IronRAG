use super::*;

#[derive(Debug, Clone, FromRow)]
pub struct DocumentCostRow {
    pub document_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct DocumentCostReadSnapshotRow {
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    document_id: Option<Uuid>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    provider_call_count: Option<i64>,
    unpriced_provider_call_count: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct LibraryCostSummaryRow {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct LibraryCostSummaryReadSnapshotRow {
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    document_count: Option<i64>,
    provider_call_count: Option<i64>,
    unpriced_provider_call_count: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkspaceCostSummaryRow {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub library_count: i64,
    pub document_count: i64,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct WorkspaceCostSummaryReadSnapshotRow {
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    library_count: Option<i64>,
    document_count: Option<i64>,
    provider_call_count: Option<i64>,
    unpriced_provider_call_count: Option<i64>,
}

/// Reads library rollup health and every document aggregate from one SQL
/// statement snapshot. Partial health indexes keep the blocker probes bounded;
/// blocked snapshots omit the stale aggregate rows entirely.
pub async fn get_document_costs_read_snapshot<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    library_id: Uuid,
) -> Result<BillingCostReadSnapshot<DocumentCostRow>, sqlx::Error> {
    // Defined shape: billing_execution_cost carries library_id +
    // knowledge_document_id directly, so per-document
    // rollup is a single indexed aggregate without the old 5-way LEFT
    // JOIN through provider_call / ingest_attempt / ingest_job /
    // runtime_graph_extraction. The old CTE also re-fanned rows through
    // billing_usage + billing_charge, which produced correct totals
    // only by accident (SUM over a LEFT JOIN that happened to have
    // zero-or-one charge per provider_call row).
    let rows = sqlx::query_as::<_, DocumentCostReadSnapshotRow>(
        "with rollup_health as materialized (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.applied_generation < rollup_state.dirty_generation
                ) as rollup_dirty,
                (
                    select rollup_state.terminal_error_code
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.terminal_error_code is not null
                    order by rollup_state.owning_execution_id
                    limit 1
                ) as terminal_error_code
         ), document_costs as (
            select
                document.id as document_id,
                coalesce(sum(execution_cost.total_cost), 0) as total_cost,
                coalesce(execution_cost.currency_code, 'USD') as currency_code,
                coalesce(sum(execution_cost.provider_call_count), 0)::bigint
                    as provider_call_count,
                coalesce(sum(execution_cost.unpriced_provider_call_count), 0)::bigint
                    as unpriced_provider_call_count,
                document.created_at
            from rollup_health health
            join content_document document
             on not health.rollup_dirty
             and health.terminal_error_code is null
             and document.library_id = $1
             and document.document_state = 'active'
             and document.deleted_at is null
            left join billing_execution_cost execution_cost
              on execution_cost.library_id = document.library_id
             and execution_cost.knowledge_document_id = document.id
            group by document.id, document.created_at, execution_cost.currency_code
         )
         select
            health.rollup_dirty,
            health.terminal_error_code,
            costs.document_id,
            costs.total_cost,
            costs.currency_code,
            costs.provider_call_count,
            costs.unpriced_provider_call_count
         from rollup_health health
         left join document_costs costs on true
         order by
            costs.total_cost desc nulls last,
            costs.created_at desc nulls last,
            costs.currency_code asc nulls last",
    )
    .bind(library_id)
    .fetch_all(executor)
    .await?;
    let first = rows.first().ok_or_else(|| {
        sqlx::Error::Protocol("billing document-cost snapshot returned no health row".to_string())
    })?;
    let mut costs = Vec::with_capacity(rows.len());
    if !first.rollup_dirty && first.terminal_error_code.is_none() {
        for row in &rows {
            let Some(document_id) = row.document_id else {
                continue;
            };
            costs.push(DocumentCostRow {
                document_id,
                total_cost: required_snapshot_field(row.total_cost, "total_cost")?,
                currency_code: required_snapshot_field(row.currency_code.clone(), "currency_code")?,
                provider_call_count: required_snapshot_field(
                    row.provider_call_count,
                    "provider_call_count",
                )?,
                unpriced_provider_call_count: required_snapshot_field(
                    row.unpriced_provider_call_count,
                    "unpriced_provider_call_count",
                )?,
            });
        }
    }
    Ok(BillingCostReadSnapshot {
        rollup_dirty: first.rollup_dirty,
        terminal_error_code: first.terminal_error_code.clone(),
        rows: costs,
    })
}

pub async fn get_library_cost_read_snapshot<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    library_id: Uuid,
) -> Result<BillingCostReadSnapshot<LibraryCostSummaryRow>, sqlx::Error> {
    // Defined shape: read the rollup table directly by library_id.
    // The previous implementation JOINed billing_execution_cost back to
    // billing_provider_call on (owning_execution_kind, owning_execution_id)
    // — billing_execution_cost is UNIQUE on that pair, but provider_call
    // has many rows per execution, so the join fanned each rollup row
    // by the number of provider_call rows, DOUBLING total_cost whenever
    // an execution had more than one provider call. Aside from being
    // ~50× more expensive, the result was numerically wrong.
    let rows = sqlx::query_as::<_, LibraryCostSummaryReadSnapshotRow>(
        "with rollup_health as materialized (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.applied_generation < rollup_state.dirty_generation
                ) as rollup_dirty,
                (
                    select rollup_state.terminal_error_code
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.terminal_error_code is not null
                    order by rollup_state.owning_execution_id
                    limit 1
                ) as terminal_error_code
         ), cost_summary as (
            select
                sum(execution_cost.total_cost) as total_cost,
                execution_cost.currency_code,
                count(distinct execution_cost.knowledge_document_id)
                    filter (where execution_cost.knowledge_document_id is not null)::bigint
                    as document_count,
                sum(execution_cost.provider_call_count)::bigint as provider_call_count,
                sum(execution_cost.unpriced_provider_call_count)::bigint
                    as unpriced_provider_call_count
            from rollup_health health
            join billing_execution_cost execution_cost
              on not health.rollup_dirty
             and health.terminal_error_code is null
             and execution_cost.library_id = $1
            group by execution_cost.currency_code
         )
         select
            health.rollup_dirty,
            health.terminal_error_code,
            summary.total_cost,
            summary.currency_code,
            summary.document_count,
            summary.provider_call_count,
            summary.unpriced_provider_call_count
         from rollup_health health
         left join cost_summary summary on true
         order by summary.currency_code asc nulls last",
    )
    .bind(library_id)
    .fetch_all(executor)
    .await?;
    let first = rows.first().ok_or_else(|| {
        sqlx::Error::Protocol("billing library-cost snapshot returned no health row".to_string())
    })?;
    let mut summaries = Vec::with_capacity(rows.len());
    if !first.rollup_dirty && first.terminal_error_code.is_none() {
        for row in &rows {
            let Some(currency_code) = row.currency_code.clone() else {
                continue;
            };
            let shared = required_scope_cost_fields(
                row.total_cost,
                row.document_count,
                row.provider_call_count,
                row.unpriced_provider_call_count,
            )?;
            summaries.push(LibraryCostSummaryRow {
                total_cost: shared.total_cost,
                currency_code,
                document_count: shared.document_count,
                provider_call_count: shared.provider_call_count,
                unpriced_provider_call_count: shared.unpriced_provider_call_count,
            });
        }
    }
    Ok(BillingCostReadSnapshot {
        rollup_dirty: first.rollup_dirty,
        terminal_error_code: first.terminal_error_code.clone(),
        rows: summaries,
    })
}

pub async fn get_workspace_cost_read_snapshot<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    workspace_id: Uuid,
) -> Result<BillingCostReadSnapshot<WorkspaceCostSummaryRow>, sqlx::Error> {
    let rows = sqlx::query_as::<_, WorkspaceCostSummaryReadSnapshotRow>(
        "with rollup_health as materialized (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.workspace_id = $1
                      and rollup_state.applied_generation < rollup_state.dirty_generation
                ) as rollup_dirty,
                (
                    select rollup_state.terminal_error_code
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.workspace_id = $1
                      and rollup_state.terminal_error_code is not null
                    order by rollup_state.owning_execution_id
                    limit 1
                ) as terminal_error_code
         ), cost_summary as (
            select
                sum(execution_cost.total_cost) as total_cost,
                execution_cost.currency_code,
                count(distinct execution_cost.library_id)::bigint as library_count,
                count(distinct execution_cost.knowledge_document_id)
                    filter (where execution_cost.knowledge_document_id is not null)::bigint
                    as document_count,
                sum(execution_cost.provider_call_count)::bigint as provider_call_count,
                sum(execution_cost.unpriced_provider_call_count)::bigint
                    as unpriced_provider_call_count
            from rollup_health health
            join billing_execution_cost execution_cost
              on not health.rollup_dirty
             and health.terminal_error_code is null
             and execution_cost.workspace_id = $1
            group by execution_cost.currency_code
         )
         select
            health.rollup_dirty,
            health.terminal_error_code,
            summary.total_cost,
            summary.currency_code,
            summary.library_count,
            summary.document_count,
            summary.provider_call_count,
            summary.unpriced_provider_call_count
         from rollup_health health
         left join cost_summary summary on true
         order by summary.currency_code asc nulls last",
    )
    .bind(workspace_id)
    .fetch_all(executor)
    .await?;
    let first = rows.first().ok_or_else(|| {
        sqlx::Error::Protocol("billing workspace-cost snapshot returned no health row".to_string())
    })?;
    let mut summaries = Vec::with_capacity(rows.len());
    if !first.rollup_dirty && first.terminal_error_code.is_none() {
        for row in &rows {
            let Some(currency_code) = row.currency_code.clone() else {
                continue;
            };
            let shared = required_scope_cost_fields(
                row.total_cost,
                row.document_count,
                row.provider_call_count,
                row.unpriced_provider_call_count,
            )?;
            summaries.push(WorkspaceCostSummaryRow {
                total_cost: shared.total_cost,
                currency_code,
                library_count: required_snapshot_field(row.library_count, "library_count")?,
                document_count: shared.document_count,
                provider_call_count: shared.provider_call_count,
                unpriced_provider_call_count: shared.unpriced_provider_call_count,
            });
        }
    }
    Ok(BillingCostReadSnapshot {
        rollup_dirty: first.rollup_dirty,
        terminal_error_code: first.terminal_error_code.clone(),
        rows: summaries,
    })
}
