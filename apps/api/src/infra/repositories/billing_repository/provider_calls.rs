use super::*;

pub async fn create_provider_call<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingProviderCall<'_>,
) -> Result<BillingProviderCallRow, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        concat!("with provider_call as (
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
            ", billing_provider_call_projection!("provider_call."), "
         from provider_call
         join rollup_state using (owning_execution_kind, owning_execution_id)"),
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
    sqlx::query_as::<_, BillingProviderCallRow>(concat!(
        "update billing_provider_call
         set call_state = $2::billing_call_state,
             completed_at = $3
         where id = $1
           and call_state = 'started'::billing_call_state
         returning
            ",
        billing_provider_call_projection!(""),
        "
"
    ))
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
    sqlx::query_as::<_, BillingProviderCallRow>(concat!(
        "select
            ",
        billing_provider_call_projection!(""),
        "
         from billing_provider_call
         where id = $1
           and call_state = 'started'::billing_call_state
         for update"
    ))
    .bind(provider_call_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_provider_call_by_id_for_update<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_call_id: Uuid,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(concat!(
        "select
            ",
        billing_provider_call_projection!(""),
        "
         from billing_provider_call
         where id = $1
         for update"
    ))
    .bind(provider_call_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_provider_call_by_id(
    postgres: &PgPool,
    provider_call_id: Uuid,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(concat!(
        "select
            ",
        billing_provider_call_projection!(""),
        "
         from billing_provider_call
         where id = $1"
    ))
    .bind(provider_call_id)
    .fetch_optional(postgres)
    .await
}

/// Increments the durable generation for a pipeline billing mutation.
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
    sqlx::query_as::<_, BillingProviderCallRow>(concat!(
        "select
            ",
        billing_provider_call_projection!(""),
        "
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
           and (
             $3::timestamptz is null
             or (started_at, id) < ($3::timestamptz, $4::uuid)
           )
         order by started_at desc, id desc
         limit $5"
    ))
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

/// Counts provider calls of one execution that recorded usage without a
/// matching catalog price. A partially priced call counts once, so the value
/// is comparable with `provider_call_count` rather than with usage rows.
pub async fn count_unpriced_provider_calls_by_execution<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_provider_call provider_call
         where provider_call.owning_execution_kind = $1::billing_owning_execution_kind
           and provider_call.owning_execution_id = $2
           and exists (
             select 1
             from billing_usage usage
             where usage.provider_call_id = provider_call.id
               and not exists (
                 select 1
                 from billing_charge charge
                 where charge.usage_id = usage.id
               )
           )",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(executor)
    .await
}
