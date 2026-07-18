use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct BillingProviderCallRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<String>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: String,
    pub call_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingUsageRow {
    pub id: Uuid,
    pub provider_call_id: Uuid,
    pub usage_kind: String,
    pub billing_unit: String,
    pub quantity: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingChargeRow {
    pub id: Uuid,
    pub usage_id: Uuid,
    pub price_catalog_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionCostRow {
    pub id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub updated_at: DateTime<Utc>,
}

/// One statement-level view of an execution's canonical billing generation
/// and its derived scalar cost. Keeping these fields in one SQL result prevents
/// a writer from committing between a health check and the aggregate read.
#[derive(Debug, Clone)]
pub struct BillingExecutionCostReadSnapshot {
    pub rollup_state_present: bool,
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub canonical_provider_call_count: i64,
    pub execution_cost: Option<BillingExecutionCostRow>,
}

/// One statement-level view of a scope's rollup health and aggregate rows.
///
/// `rows` is empty while the scope is dirty or terminal, making it impossible
/// for a caller to accidentally expose an older scalar total after ignoring
/// the health fields.
#[derive(Debug, Clone)]
pub struct BillingCostReadSnapshot<T> {
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub rows: Vec<T>,
}

#[derive(Debug, Clone, FromRow)]
struct BillingExecutionCostReadSnapshotRow {
    rollup_state_present: bool,
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    canonical_provider_call_count: i64,
    id: Option<Uuid>,
    owning_execution_kind: Option<String>,
    owning_execution_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    knowledge_document_id: Option<Uuid>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    provider_call_count: Option<i32>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionCostRollupRow {
    pub currency_code: String,
    pub total_cost: Decimal,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionProviderCallDescriptorRow {
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
}

/// Durable generation cursor for one derived execution-cost aggregate.
///
/// Canonical billing writes increment `dirty_generation` in their own
/// transaction. A rollup may advance `applied_generation` only while the
/// observed dirty generation is unchanged, so a concurrent completion can
/// never be acknowledged by an aggregate that did not observe it.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct BillingExecutionCostRollupStateRow {
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub dirty_generation: i64,
    pub applied_generation: i64,
    pub dirty_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
    pub repair_attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub terminal_error_code: Option<String>,
}

/// One unique execution affected by an atomic stale-reservation sweep.
/// `reaped_provider_calls` is bounded by [`MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT`]
/// across the complete result set.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct ReapedBillingProviderCallExecutionRow {
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub reaped_provider_calls: i64,
}

/// Repository-level safety ceiling. Maintenance callers may request a smaller
/// batch, but can never turn one sweep into an unbounded write or rollup fanout.
pub const MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT: i64 = 100;

/// Repository-level safety ceiling for one durable rollup-repair sweep.
pub const MAX_DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT: i64 = 100;

#[derive(Debug, Clone)]
pub struct NewBillingProviderCall<'a> {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: &'a str,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<&'a str>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: &'a str,
    pub call_state: &'a str,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewBillingUsage<'a> {
    pub provider_call_id: Uuid,
    pub usage_kind: &'a str,
    pub billing_unit: &'a str,
    pub quantity: Decimal,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewBillingCharge {
    pub usage_id: Uuid,
    pub price_catalog_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpsertBillingExecutionCost<'a> {
    pub owning_execution_kind: &'a str,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
    pub total_cost: Decimal,
    pub currency_code: &'a str,
    pub provider_call_count: i32,
}

pub async fn create_provider_call<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingProviderCall<'_>,
) -> Result<BillingProviderCallRow, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "with provider_call as (
            insert into billing_provider_call (
                id,
                workspace_id,
                library_id,
                binding_id,
                owning_execution_kind,
                owning_execution_id,
                runtime_execution_id,
                runtime_task_kind,
                provider_catalog_id,
                model_catalog_id,
                call_kind,
                started_at,
                completed_at,
                call_state
            )
            values ($1, $2, $3, $4, $5::billing_owning_execution_kind, $6, $7, $8::runtime_task_kind, $9, $10, $11, now(), $12, $13::billing_call_state)
            returning *
         ), rollup_state as (
            insert into billing_execution_cost_rollup_state (
                owning_execution_kind,
                owning_execution_id,
                workspace_id,
                library_id,
                dirty_generation,
                applied_generation,
                dirty_at,
                repair_attempts,
                next_attempt_at,
                last_error
            )
            select
                provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
                provider_call.workspace_id,
                provider_call.library_id,
                1,
                0,
                now(),
                0,
                now(),
                null
            from provider_call
            on conflict (owning_execution_kind, owning_execution_id)
            do update set
                workspace_id = excluded.workspace_id,
                library_id = excluded.library_id,
                dirty_generation = billing_execution_cost_rollup_state.dirty_generation + 1,
                dirty_at = now(),
                repair_attempts = 0,
                next_attempt_at = now(),
                last_error = null,
                terminal_error_code = null
            returning owning_execution_kind, owning_execution_id
         )
         select
            provider_call.id,
            provider_call.workspace_id,
            provider_call.library_id,
            provider_call.binding_id,
            provider_call.owning_execution_kind::text as owning_execution_kind,
            provider_call.owning_execution_id,
            provider_call.runtime_execution_id,
            provider_call.runtime_task_kind::text as runtime_task_kind,
            provider_call.provider_catalog_id,
            provider_call.model_catalog_id,
            provider_call.call_kind,
            provider_call.call_state::text as call_state,
            provider_call.started_at,
            provider_call.completed_at
         from provider_call
         join rollup_state using (owning_execution_kind, owning_execution_id)",
    )
    .bind(input.id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.binding_id)
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.runtime_execution_id)
    .bind(input.runtime_task_kind)
    .bind(input.provider_catalog_id)
    .bind(input.model_catalog_id)
    .bind(input.call_kind)
    .bind(input.completed_at)
    .bind(input.call_state)
    .fetch_one(executor)
    .await
}

pub async fn update_provider_call_state<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_call_id: Uuid,
    call_state: &str,
    completed_at: Option<DateTime<Utc>>,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "update billing_provider_call
         set call_state = $2::billing_call_state,
             completed_at = $3
         where id = $1
           and call_state = 'started'::billing_call_state
         returning
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at",
    )
    .bind(provider_call_id)
    .bind(call_state)
    .bind(completed_at)
    .fetch_optional(executor)
    .await
}

pub async fn get_started_provider_call_by_id_for_update<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_call_id: Uuid,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at
         from billing_provider_call
         where id = $1
           and call_state = 'started'::billing_call_state
         for update",
    )
    .bind(provider_call_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_provider_call_by_id_for_update<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_call_id: Uuid,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at
         from billing_provider_call
         where id = $1
         for update",
    )
    .bind(provider_call_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_provider_call_by_id(
    postgres: &PgPool,
    provider_call_id: Uuid,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at
         from billing_provider_call
         where id = $1",
    )
    .bind(provider_call_id)
    .fetch_optional(postgres)
    .await
}

/// Increments the durable generation for a canonical billing mutation.
///
/// This must run on the same transaction as the provider-call state, usage,
/// or charge mutation it describes. Replaying it is safe: an extra generation
/// causes only an idempotent aggregate rebuild.
pub async fn mark_execution_cost_rollup_dirty<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_call: &BillingProviderCallRow,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "insert into billing_execution_cost_rollup_state (
            owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            dirty_generation,
            applied_generation,
            dirty_at,
            repair_attempts,
            next_attempt_at,
            last_error
         )
         values (
            $1::billing_owning_execution_kind,
            $2,
            $3,
            $4,
            1,
            0,
            now(),
            0,
            now(),
            null
         )
         on conflict (owning_execution_kind, owning_execution_id)
         do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            dirty_generation = billing_execution_cost_rollup_state.dirty_generation + 1,
            dirty_at = now(),
            repair_attempts = 0,
            next_attempt_at = now(),
            last_error = null,
            terminal_error_code = null
         returning dirty_generation",
    )
    .bind(&provider_call.owning_execution_kind)
    .bind(provider_call.owning_execution_id)
    .bind(provider_call.workspace_id)
    .bind(provider_call.library_id)
    .fetch_one(executor)
    .await
}

/// Atomically cancels a bounded batch of abandoned provider-call reservations.
///
/// Candidate rows are locked with `SKIP LOCKED`, so multiple scheduler nodes
/// can sweep concurrently without waiting on or relabeling each other's work.
/// The repeated `call_state = 'started'` predicate on the update protects a
/// legitimate completion that wins the row lock before this statement.
pub async fn reap_stale_started_provider_calls(
    postgres: &PgPool,
    stale_after: std::time::Duration,
    batch_limit: i64,
) -> Result<Vec<ReapedBillingProviderCallExecutionRow>, sqlx::Error> {
    if batch_limit <= 0 {
        return Ok(Vec::new());
    }
    let batch_limit = batch_limit.min(MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT);

    sqlx::query_as::<_, ReapedBillingProviderCallExecutionRow>(
        "with candidates as (
            select candidate.id
            from billing_provider_call candidate
            where candidate.call_state = 'started'::billing_call_state
              and candidate.started_at <
                  now() - make_interval(secs => $1::double precision)
            order by candidate.started_at asc, candidate.id asc
            for update skip locked
            limit $2
         ), canceled as (
            update billing_provider_call provider_call
            set call_state = 'canceled'::billing_call_state,
                completed_at = now()
            from candidates
            where provider_call.id = candidates.id
              and provider_call.call_state = 'started'::billing_call_state
            returning
                provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
                provider_call.workspace_id,
                provider_call.library_id
         ), canceled_grouped as (
            select
                owning_execution_kind,
                owning_execution_id,
                workspace_id,
                library_id,
                count(*)::bigint as reaped_provider_calls
            from canceled
            group by
                owning_execution_kind,
                owning_execution_id,
                workspace_id,
                library_id
         ), rollup_state as (
            insert into billing_execution_cost_rollup_state (
                owning_execution_kind,
                owning_execution_id,
                workspace_id,
                library_id,
                dirty_generation,
                applied_generation,
                dirty_at,
                repair_attempts,
                next_attempt_at,
                last_error
            )
            select
                owning_execution_kind,
                owning_execution_id,
                workspace_id,
                library_id,
                1,
                0,
                now(),
                0,
                now(),
                null
            from canceled_grouped
            on conflict (owning_execution_kind, owning_execution_id)
            do update set
                workspace_id = excluded.workspace_id,
                library_id = excluded.library_id,
                dirty_generation = billing_execution_cost_rollup_state.dirty_generation + 1,
                dirty_at = now(),
                repair_attempts = 0,
                next_attempt_at = now(),
                last_error = null,
                terminal_error_code = null
            returning owning_execution_kind, owning_execution_id
         )
         select
            canceled_grouped.owning_execution_kind::text as owning_execution_kind,
            canceled_grouped.owning_execution_id,
            canceled_grouped.reaped_provider_calls
         from canceled_grouped
         join rollup_state using (owning_execution_kind, owning_execution_id)
         order by
            canceled_grouped.owning_execution_kind,
            canceled_grouped.owning_execution_id",
    )
    .bind(stale_after.as_secs_f64())
    .bind(batch_limit)
    .fetch_all(postgres)
    .await
}

/// Keyset page over one execution's provider calls, ordered newest-first
/// (`started_at desc, id desc`).
/// `cursor` is the `(started_at, id)` of the last row of the previous page —
/// `None` starts from the top. Fetches `limit + 1` rows so the caller can
/// derive `has_more` without a second round trip; callers must trim the
/// extra row before returning it to clients.
pub async fn list_provider_calls_by_execution_page(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<BillingProviderCallRow>, sqlx::Error> {
    let (cursor_started_at, cursor_id) = cursor.unzip();
    sqlx::query_as::<_, BillingProviderCallRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
           and (
             $3::timestamptz is null
             or (started_at, id) < ($3::timestamptz, $4::uuid)
           )
         order by started_at desc, id desc
         limit $5",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(cursor_started_at)
    .bind(cursor_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}

pub async fn list_provider_call_descriptors_by_execution_ids(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_ids: &[Uuid],
) -> Result<Vec<BillingExecutionProviderCallDescriptorRow>, sqlx::Error> {
    if owning_execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, BillingExecutionProviderCallDescriptorRow>(
        "select
            bpc.owning_execution_id,
            bpc.runtime_execution_id,
            apc.provider_kind,
            amc.model_name
         from billing_provider_call bpc
         join ai_provider_catalog apc on apc.id = bpc.provider_catalog_id
         join ai_model_catalog amc on amc.id = bpc.model_catalog_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = any($2)
         order by bpc.owning_execution_id asc, bpc.started_at desc, bpc.id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_ids)
    .fetch_all(postgres)
    .await
}

pub async fn create_usage<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingUsage<'_>,
) -> Result<BillingUsageRow, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "insert into billing_usage (
            id,
            provider_call_id,
            usage_kind,
            billing_unit,
            quantity,
            observed_at
        )
        values ($1, $2, $3, $4::billing_unit, $5, coalesce($6, now()))
        returning
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.provider_call_id)
    .bind(input.usage_kind)
    .bind(input.billing_unit)
    .bind(input.quantity)
    .bind(input.observed_at)
    .fetch_one(executor)
    .await
}

pub async fn list_usage_by_provider_call(
    postgres: &PgPool,
    provider_call_id: Uuid,
) -> Result<Vec<BillingUsageRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "select
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at
         from billing_usage
         where provider_call_id = $1
         order by observed_at asc, id asc",
    )
    .bind(provider_call_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_charge<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingCharge,
) -> Result<BillingChargeRow, sqlx::Error> {
    sqlx::query_as::<_, BillingChargeRow>(
        "insert into billing_charge (
            id,
            usage_id,
            price_catalog_id,
            currency_code,
            unit_price,
            total_price,
            priced_at
        )
        values ($1, $2, $3, $4, $5, $6, coalesce($7, now()))
        returning
            id,
            usage_id,
            price_catalog_id,
            currency_code,
            unit_price,
            total_price,
            priced_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.usage_id)
    .bind(input.price_catalog_id)
    .bind(&input.currency_code)
    .bind(input.unit_price)
    .bind(input.total_price)
    .bind(input.priced_at)
    .fetch_one(executor)
    .await
}

/// Keyset page over one execution's billing charges, ordered newest-first
/// (`priced_at desc, id desc`).
/// `cursor` is the `(priced_at, id)` of the last row of the previous page —
/// `None` starts from the top. Fetches `limit + 1` rows so the caller can
/// derive `has_more` without a second round trip; callers must trim the
/// extra row before returning it to clients.
pub async fn list_charges_by_execution_page(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<BillingChargeRow>, sqlx::Error> {
    let (cursor_priced_at, cursor_id) = cursor.unzip();
    sqlx::query_as::<_, BillingChargeRow>(
        "select
            bc.id,
            bc.usage_id,
            bc.price_catalog_id,
            bc.currency_code,
            bc.unit_price,
            bc.total_price,
            bc.priced_at
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2
           and (
             $3::timestamptz is null
             or (bc.priced_at, bc.id) < ($3::timestamptz, $4::uuid)
           )
         order by bc.priced_at desc, bc.id desc
         limit $5",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(cursor_priced_at)
    .bind(cursor_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}

/// Total number of billing charges attributed to one execution, for the
/// optional `total` field on the paginated charges envelope.
pub async fn count_charges_by_execution<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(executor)
    .await
}

pub async fn upsert_execution_cost<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &UpsertBillingExecutionCost<'_>,
) -> Result<BillingExecutionCostRow, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRow>(
        "insert into billing_execution_cost (
            id,
            owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            knowledge_document_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at
        )
        values ($1, $2::billing_owning_execution_kind, $3, $4, $5, $6, $7, $8, $9, now())
        on conflict (owning_execution_kind, owning_execution_id)
        do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            knowledge_document_id = excluded.knowledge_document_id,
            total_cost = excluded.total_cost,
            currency_code = excluded.currency_code,
            provider_call_count = excluded.provider_call_count,
            updated_at = now()
        returning
            id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            knowledge_document_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.knowledge_document_id)
    .bind(input.total_cost)
    .bind(input.currency_code)
    .bind(input.provider_call_count)
    .fetch_one(executor)
    .await
}

/// Reads the durable generation cursor, canonical provider-call count, and
/// derived execution cost in one Postgres statement snapshot.
pub async fn get_execution_cost_read_snapshot<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<BillingExecutionCostReadSnapshot, sqlx::Error> {
    let row = sqlx::query_as::<_, BillingExecutionCostReadSnapshotRow>(
        "select
            rollup_state.owning_execution_id is not null as rollup_state_present,
            coalesce(
                rollup_state.applied_generation < rollup_state.dirty_generation,
                false
            ) as rollup_dirty,
            rollup_state.terminal_error_code,
            provider_calls.canonical_provider_call_count,
            execution_cost.id,
            execution_cost.owning_execution_kind::text as owning_execution_kind,
            execution_cost.owning_execution_id,
            execution_cost.workspace_id,
            execution_cost.library_id,
            execution_cost.knowledge_document_id,
            execution_cost.total_cost,
            execution_cost.currency_code,
            execution_cost.provider_call_count,
            execution_cost.updated_at
         from (values (true)) as anchor(present)
         left join billing_execution_cost_rollup_state rollup_state
           on rollup_state.owning_execution_kind = $1::billing_owning_execution_kind
          and rollup_state.owning_execution_id = $2
         left join billing_execution_cost execution_cost
           on execution_cost.owning_execution_kind = $1::billing_owning_execution_kind
          and execution_cost.owning_execution_id = $2
         cross join lateral (
            select count(*)::bigint as canonical_provider_call_count
            from billing_provider_call provider_call
            where provider_call.owning_execution_kind = $1::billing_owning_execution_kind
              and provider_call.owning_execution_id = $2
         ) provider_calls
         where anchor.present",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(executor)
    .await?;

    let execution_cost = match row.id {
        None => None,
        Some(id) => Some(BillingExecutionCostRow {
            id,
            owning_execution_kind: required_snapshot_field(
                row.owning_execution_kind,
                "owning_execution_kind",
            )?,
            owning_execution_id: required_snapshot_field(
                row.owning_execution_id,
                "owning_execution_id",
            )?,
            workspace_id: required_snapshot_field(row.workspace_id, "workspace_id")?,
            library_id: required_snapshot_field(row.library_id, "library_id")?,
            knowledge_document_id: row.knowledge_document_id,
            total_cost: required_snapshot_field(row.total_cost, "total_cost")?,
            currency_code: required_snapshot_field(row.currency_code, "currency_code")?,
            provider_call_count: required_snapshot_field(
                row.provider_call_count,
                "provider_call_count",
            )?,
            updated_at: required_snapshot_field(row.updated_at, "updated_at")?,
        }),
    };

    Ok(BillingExecutionCostReadSnapshot {
        rollup_state_present: row.rollup_state_present,
        rollup_dirty: row.rollup_dirty,
        terminal_error_code: row.terminal_error_code,
        canonical_provider_call_count: row.canonical_provider_call_count,
        execution_cost,
    })
}

fn required_snapshot_field<T>(value: Option<T>, field: &str) -> Result<T, sqlx::Error> {
    value.ok_or_else(|| {
        sqlx::Error::Protocol(format!(
            "billing snapshot returned a partial aggregate row: missing {field}"
        ))
    })
}

/// Lists only generation-clean, non-terminal scalar execution costs. A dirty
/// or mixed-currency execution is omitted so batch/audit callers cannot expose
/// a stale total while canonical billing is ahead of the derived row.
pub async fn list_execution_costs_by_execution_ids(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_ids: &[Uuid],
) -> Result<Vec<BillingExecutionCostRow>, sqlx::Error> {
    if owning_execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, BillingExecutionCostRow>(
        "select
            execution_cost.id,
            execution_cost.owning_execution_kind::text as owning_execution_kind,
            execution_cost.owning_execution_id,
            execution_cost.workspace_id,
            execution_cost.library_id,
            execution_cost.knowledge_document_id,
            execution_cost.total_cost,
            execution_cost.currency_code,
            execution_cost.provider_call_count,
            execution_cost.updated_at
         from billing_execution_cost execution_cost
         join billing_execution_cost_rollup_state rollup_state
           on rollup_state.owning_execution_kind = execution_cost.owning_execution_kind
          and rollup_state.owning_execution_id = execution_cost.owning_execution_id
          and rollup_state.applied_generation = rollup_state.dirty_generation
          and rollup_state.terminal_error_code is null
         where execution_cost.owning_execution_kind =
               $1::billing_owning_execution_kind
           and execution_cost.owning_execution_id = any($2)
         order by execution_cost.updated_at desc, execution_cost.id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_ids)
    .fetch_all(postgres)
    .await
}

pub async fn list_execution_cost_rollups<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Vec<BillingExecutionCostRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRollupRow>(
        "select
            bc.currency_code,
            sum(bc.total_price) as total_cost,
            count(distinct bpc.id)::bigint as provider_call_count
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2
         group by bc.currency_code
         order by bc.currency_code asc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_all(executor)
    .await
}

pub async fn ensure_execution_cost_rollup_state<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<BillingExecutionCostRollupStateRow, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRollupStateRow>(
        "insert into billing_execution_cost_rollup_state (
            owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            dirty_generation,
            applied_generation,
            dirty_at,
            repair_attempts,
            next_attempt_at,
            last_error
         )
         values (
            $1::billing_owning_execution_kind,
            $2,
            $3,
            $4,
            1,
            0,
            now(),
            0,
            now(),
            null
         )
         on conflict (owning_execution_kind, owning_execution_id)
         do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id
         returning
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            dirty_generation,
            applied_generation,
            dirty_at,
            applied_at,
            repair_attempts,
            next_attempt_at,
            last_error,
            terminal_error_code",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_one(executor)
    .await
}

/// Acknowledges exactly the canonical generation used for an aggregate.
/// Returns `false` when a concurrent canonical mutation incremented the dirty
/// generation after the aggregate read began.
pub async fn acknowledge_execution_cost_rollup_generation<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    dirty_generation: i64,
) -> Result<bool, sqlx::Error> {
    let acknowledged = sqlx::query_scalar::<_, i64>(
        "update billing_execution_cost_rollup_state
         set applied_generation = $3,
             applied_at = now(),
             repair_attempts = 0,
             next_attempt_at = now(),
             last_error = null,
             terminal_error_code = null
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
           and dirty_generation = $3
         returning applied_generation",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(dirty_generation)
    .fetch_optional(executor)
    .await?;
    Ok(acknowledged.is_some())
}

/// Acknowledges one generation as permanently unrepresentable by the scalar
/// execution-cost row. Terminal acknowledgement removes the generation from
/// the retry queue while preserving a typed blocker for every aggregate read.
pub async fn acknowledge_execution_cost_rollup_terminal_error<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    dirty_generation: i64,
    terminal_error_code: &str,
) -> Result<bool, sqlx::Error> {
    let acknowledged = sqlx::query_scalar::<_, i64>(
        "update billing_execution_cost_rollup_state
         set applied_generation = $3,
             applied_at = now(),
             repair_attempts = 0,
             next_attempt_at = now(),
             last_error = null,
             terminal_error_code = $4
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
           and dirty_generation = $3
         returning applied_generation",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(dirty_generation)
    .bind(terminal_error_code)
    .fetch_optional(executor)
    .await?;
    Ok(acknowledged.is_some())
}

pub async fn get_execution_cost_rollup_state(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Option<BillingExecutionCostRollupStateRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRollupStateRow>(
        "select
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            dirty_generation,
            applied_generation,
            dirty_at,
            applied_at,
            repair_attempts,
            next_attempt_at,
            last_error,
            terminal_error_code
         from billing_execution_cost_rollup_state
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_optional(postgres)
    .await
}

/// Atomically claims a bounded batch of due repairs with `SKIP LOCKED`.
///
/// `next_attempt_at` doubles as a recoverable soft lease: another worker may
/// retry after the database-clock lease expires if this process exits before
/// acknowledge/failure.
pub async fn claim_due_execution_cost_rollup_repairs(
    postgres: &PgPool,
    batch_limit: i64,
    claim_lease: std::time::Duration,
) -> Result<Vec<BillingExecutionCostRollupStateRow>, sqlx::Error> {
    if batch_limit <= 0 {
        return Ok(Vec::new());
    }
    let batch_limit = batch_limit.min(MAX_DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT);
    sqlx::query_as::<_, BillingExecutionCostRollupStateRow>(
        "with candidates as (
            select owning_execution_kind, owning_execution_id
            from billing_execution_cost_rollup_state
            where applied_generation < dirty_generation
              and next_attempt_at <= now()
            order by next_attempt_at asc, dirty_at asc, owning_execution_id asc
            for update skip locked
            limit $1
         ), claimed as (
            update billing_execution_cost_rollup_state rollup_state
            set next_attempt_at =
                now() + make_interval(secs => $2::double precision)
            from candidates
            where rollup_state.owning_execution_kind = candidates.owning_execution_kind
              and rollup_state.owning_execution_id = candidates.owning_execution_id
              and rollup_state.applied_generation < rollup_state.dirty_generation
            returning rollup_state.*
         )
         select
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            dirty_generation,
            applied_generation,
            dirty_at,
            applied_at,
            repair_attempts,
            next_attempt_at,
            last_error,
            terminal_error_code
         from claimed
         order by dirty_at asc, owning_execution_id asc",
    )
    .bind(batch_limit)
    .bind(claim_lease.as_secs_f64())
    .fetch_all(postgres)
    .await
}

pub async fn record_execution_cost_rollup_failure(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    dirty_generation: i64,
    retry_after: std::time::Duration,
    error_kind: &str,
) -> Result<bool, sqlx::Error> {
    let recorded = sqlx::query_scalar::<_, i64>(
        "update billing_execution_cost_rollup_state
         set repair_attempts = repair_attempts + 1,
             next_attempt_at =
                 now() + make_interval(secs => $4::double precision),
             last_error = left($5, 512)
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
           and dirty_generation = $3
           and applied_generation < dirty_generation
         returning dirty_generation",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(dirty_generation)
    .bind(retry_after.as_secs_f64())
    .bind(error_kind)
    .fetch_optional(postgres)
    .await?;
    Ok(recorded.is_some())
}

/// Removes a stale repair cursor after its last canonical provider call was
/// deleted (normally through execution/library cascade cleanup). The
/// generation predicate prevents an old maintenance claim from deleting
/// concurrently recreated work.
pub async fn delete_orphaned_execution_cost_rollup_state(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    dirty_generation: i64,
) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query_scalar::<_, i64>(
        "delete from billing_execution_cost_rollup_state rollup_state
         where rollup_state.owning_execution_kind = $1::billing_owning_execution_kind
           and rollup_state.owning_execution_id = $2
           and rollup_state.dirty_generation = $3
           and not exists (
                select 1
                from billing_provider_call provider_call
                where provider_call.owning_execution_kind = rollup_state.owning_execution_kind
                  and provider_call.owning_execution_id = rollup_state.owning_execution_id
           )
         returning dirty_generation",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(dirty_generation)
    .fetch_optional(postgres)
    .await?;
    Ok(deleted.is_some())
}

#[derive(Debug, Clone, FromRow)]
pub struct DocumentCostRow {
    pub document_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct DocumentCostReadSnapshotRow {
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    document_id: Option<Uuid>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    provider_call_count: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct LibraryCostSummaryRow {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct LibraryCostSummaryReadSnapshotRow {
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    document_count: Option<i64>,
    provider_call_count: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WorkspaceCostSummaryRow {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub library_count: i64,
    pub document_count: i64,
    pub provider_call_count: i64,
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
}

/// Reads library rollup health and every document aggregate from one SQL
/// statement snapshot. Partial health indexes keep the blocker probes bounded;
/// blocked snapshots omit the stale aggregate rows entirely.
pub async fn get_document_costs_read_snapshot<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    library_id: Uuid,
) -> Result<BillingCostReadSnapshot<DocumentCostRow>, sqlx::Error> {
    // Canonical shape: billing_execution_cost carries library_id +
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
                document.created_at
            from rollup_health health
            join content_document document
              on not health.rollup_dirty
             and health.terminal_error_code is null
             and document.library_id = $1
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
            costs.provider_call_count
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
    // Canonical shape: read the rollup table directly by library_id.
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
                sum(execution_cost.provider_call_count)::bigint as provider_call_count
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
            summary.provider_call_count
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
            summaries.push(LibraryCostSummaryRow {
                total_cost: required_snapshot_field(row.total_cost, "total_cost")?,
                currency_code,
                document_count: required_snapshot_field(row.document_count, "document_count")?,
                provider_call_count: required_snapshot_field(
                    row.provider_call_count,
                    "provider_call_count",
                )?,
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
                sum(execution_cost.provider_call_count)::bigint as provider_call_count
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
            summary.provider_call_count
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
            summaries.push(WorkspaceCostSummaryRow {
                total_cost: required_snapshot_field(row.total_cost, "total_cost")?,
                currency_code,
                library_count: required_snapshot_field(row.library_count, "library_count")?,
                document_count: required_snapshot_field(row.document_count, "document_count")?,
                provider_call_count: required_snapshot_field(
                    row.provider_call_count,
                    "provider_call_count",
                )?,
            });
        }
    }
    Ok(BillingCostReadSnapshot {
        rollup_dirty: first.rollup_dirty,
        terminal_error_code: first.terminal_error_code.clone(),
        rows: summaries,
    })
}

pub async fn count_provider_calls_by_execution<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(executor)
    .await
}

/// Serializes the derived cost refresh for one logical execution. Callers must
/// invoke this on the same transaction used for the aggregate reads and
/// upsert; the xact-scoped lock is released automatically on commit/rollback.
pub async fn lock_execution_cost_rollup<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<(), sqlx::Error> {
    let lock_identity = format!("{owning_execution_kind}:{owning_execution_id}");
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_identity)
        .execute(executor)
        .await?;
    Ok(())
}
