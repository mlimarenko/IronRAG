use super::*;

pub async fn upsert_execution_cost<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &UpsertBillingExecutionCost<'_>,
) -> Result<BillingExecutionCostRow, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRow>(concat!(
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
            unpriced_provider_call_count,
            updated_at
        )
        values ($1, $2::billing_owning_execution_kind, $3, $4, $5, $6, $7, $8, $9, $10, now())
        on conflict (owning_execution_kind, owning_execution_id)
        do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            knowledge_document_id = excluded.knowledge_document_id,
            total_cost = excluded.total_cost,
            currency_code = excluded.currency_code,
            provider_call_count = excluded.provider_call_count,
            unpriced_provider_call_count = excluded.unpriced_provider_call_count,
            updated_at = now()
        returning
            ",
        billing_execution_cost_projection!(""),
        "
"
    ))
    .bind(Uuid::now_v7())
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.knowledge_document_id)
    .bind(input.total_cost)
    .bind(input.currency_code)
    .bind(input.provider_call_count)
    .bind(input.unpriced_provider_call_count)
    .fetch_one(executor)
    .await
}

/// Reads the durable generation cursor, tracked provider-call count, and
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
            provider_calls.grounded_answer_provider_call_count,
            execution_cost.id,
            execution_cost.owning_execution_kind::text as owning_execution_kind,
            execution_cost.owning_execution_id,
            execution_cost.workspace_id,
            execution_cost.library_id,
            execution_cost.knowledge_document_id,
            execution_cost.total_cost,
            execution_cost.currency_code,
            execution_cost.provider_call_count,
            execution_cost.unpriced_provider_call_count,
            execution_cost.updated_at
         from (values (true)) as anchor(present)
         left join billing_execution_cost_rollup_state rollup_state
           on rollup_state.owning_execution_kind = $1::billing_owning_execution_kind
          and rollup_state.owning_execution_id = $2
         left join billing_execution_cost execution_cost
           on execution_cost.owning_execution_kind = $1::billing_owning_execution_kind
          and execution_cost.owning_execution_id = $2
         cross join lateral (
            select count(*)::bigint as grounded_answer_provider_call_count
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
            unpriced_provider_call_count: required_snapshot_field(
                row.unpriced_provider_call_count,
                "unpriced_provider_call_count",
            )?,
            updated_at: required_snapshot_field(row.updated_at, "updated_at")?,
        }),
    };

    Ok(BillingExecutionCostReadSnapshot {
        rollup_state_present: row.rollup_state_present,
        rollup_dirty: row.rollup_dirty,
        terminal_error_code: row.terminal_error_code,
        grounded_answer_provider_call_count: row.grounded_answer_provider_call_count,
        execution_cost,
    })
}

/// Lists only generation-clean, non-terminal scalar execution costs. A dirty
/// or mixed-currency execution is omitted so batch/audit callers cannot expose
/// a stale total while pipeline billing is ahead of the derived row.
pub async fn list_execution_costs_by_execution_ids(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_ids: &[Uuid],
) -> Result<Vec<BillingExecutionCostRow>, sqlx::Error> {
    if owning_execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, BillingExecutionCostRow>(concat!(
        "select
            ",
        billing_execution_cost_projection!("execution_cost."),
        "
         from billing_execution_cost execution_cost
         join billing_execution_cost_rollup_state rollup_state
           on rollup_state.owning_execution_kind = execution_cost.owning_execution_kind
          and rollup_state.owning_execution_id = execution_cost.owning_execution_id
          and rollup_state.applied_generation = rollup_state.dirty_generation
          and rollup_state.terminal_error_code is null
         where execution_cost.owning_execution_kind =
               $1::billing_owning_execution_kind
           and execution_cost.owning_execution_id = any($2)
         order by execution_cost.updated_at desc, execution_cost.id desc"
    ))
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

/// Acknowledges exactly the exact generation used for an aggregate.
/// Returns `false` when a concurrent aggregate mutation incremented the dirty
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

/// Removes a stale repair cursor after its last active provider call was
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
