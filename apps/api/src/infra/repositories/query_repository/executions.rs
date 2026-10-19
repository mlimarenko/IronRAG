use super::*;

pub async fn list_executions_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRowRecord>(query_execution_runtime_sql!(
        " where execution.conversation_id = $1
          order by execution.started_at desc, execution.id desc"
    ))
    .bind(conversation_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_execution_row)
    .collect()
}

/// Identity and scope of one execution, read straight from `query_execution`.
///
/// [`get_execution_by_id`] inner-joins `runtime_execution`, so it reports a row
/// as absent whenever the runtime companion is not yet visible — a failure for
/// a reason the caller may not care about. Callers that only need to know which
/// workspace, library and runtime execution a row belongs to must not be
/// coupled to that join: on stage it turned ten present child executions into
/// "execution row missing" and cost those turns their citations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryExecutionScopeRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub runtime_execution_id: Uuid,
}

pub async fn get_execution_scope_by_id(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Option<QueryExecutionScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid)>(
        "select id, workspace_id, library_id, runtime_execution_id
           from query_execution
          where id = $1",
    )
    .bind(execution_id)
    .fetch_optional(postgres)
    .await
    .map(|row| {
        row.map(|(id, workspace_id, library_id, runtime_execution_id)| QueryExecutionScopeRow {
            id,
            workspace_id,
            library_id,
            runtime_execution_id,
        })
    })
}

pub async fn get_execution_by_id(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRowRecord>(query_execution_runtime_sql!(
        " where execution.id = $1"
    ))
    .bind(execution_id)
    .fetch_optional(postgres)
    .await?
    .map(map_query_execution_row)
    .transpose()
}

pub async fn list_executions_by_ids(
    postgres: &PgPool,
    execution_ids: &[Uuid],
) -> Result<Vec<QueryExecutionRow>, sqlx::Error> {
    if execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, QueryExecutionRowRecord>(query_execution_runtime_sql!(
        " where execution.id = any($1)
          order by execution.started_at desc, execution.id desc"
    ))
    .bind(execution_ids)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_execution_row)
    .collect()
}

pub async fn create_execution(
    postgres: &PgPool,
    input: &NewQueryExecution<'_>,
) -> Result<QueryExecutionRow, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryExecutionRowRecord>(
        "with inserted as (
            insert into query_execution (
                id,
                workspace_id,
                library_id,
                conversation_id,
                context_bundle_id,
                request_turn_id,
                response_turn_id,
                binding_id,
                runtime_execution_id,
                query_text,
                failure_code,
                started_at
            )
            select
                $1, $2, $3, $4, $5, $6, $7, $8, runtime.id,
                $10, $11, now()
            from runtime_execution runtime
            where runtime.id = $9
              and runtime.owner_kind = 'query_execution'
              and runtime.owner_id = $1
            returning *
        )
        select
            inserted.id,
            inserted.context_bundle_id,
            inserted.workspace_id,
            inserted.library_id,
            inserted.conversation_id,
            inserted.request_turn_id,
            inserted.response_turn_id,
            inserted.binding_id,
            inserted.runtime_execution_id,
            runtime.lifecycle_state::text as runtime_lifecycle_state_text,
            runtime.active_stage::text as runtime_active_stage_text,
            runtime.turn_budget,
            runtime.turn_count,
            runtime.parallel_action_limit,
            inserted.query_text,
            coalesce(runtime.failure_code, inserted.failure_code) as failure_code,
            runtime.failure_summary_redacted,
            inserted.started_at,
            runtime.accepted_at as runtime_accepted_at,
            runtime.completed_at as runtime_completed_at
        from inserted
        join runtime_execution runtime
          on runtime.id = inserted.runtime_execution_id
         and runtime.owner_kind = 'query_execution'
         and runtime.owner_id = inserted.id",
    )
    .bind(input.execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.context_bundle_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.binding_id)
    .bind(input.runtime_execution_id)
    .bind(input.query_text)
    .bind(input.failure_code)
    .fetch_one(postgres)
    .await?;
    map_query_execution_row(row)
}

pub async fn update_execution(
    postgres: &PgPool,
    execution_id: Uuid,
    input: &UpdateQueryExecution,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryExecutionRowRecord>(
        "with updated as (
            update query_execution
             set request_turn_id = $2,
                 response_turn_id = $3
             where id = $1
             returning *
        )
        select
            updated.id,
            updated.context_bundle_id,
            updated.workspace_id,
            updated.library_id,
            updated.conversation_id,
            updated.request_turn_id,
            updated.response_turn_id,
            updated.binding_id,
            updated.runtime_execution_id,
            runtime.lifecycle_state::text as runtime_lifecycle_state_text,
            runtime.active_stage::text as runtime_active_stage_text,
            runtime.turn_budget,
            runtime.turn_count,
            runtime.parallel_action_limit,
            updated.query_text,
            coalesce(runtime.failure_code, updated.failure_code) as failure_code,
            runtime.failure_summary_redacted,
            updated.started_at,
            runtime.accepted_at as runtime_accepted_at,
            runtime.completed_at as runtime_completed_at
        from updated
        join runtime_execution runtime
          on runtime.id = updated.runtime_execution_id
         and runtime.owner_kind = 'query_execution'
         and runtime.owner_id = updated.id",
    )
    .bind(execution_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .fetch_optional(postgres)
    .await?;
    row.map(map_query_execution_row).transpose()
}

pub async fn cancel_interrupted_execution(
    postgres: &PgPool,
    execution_id: Uuid,
    runtime_execution_id: Uuid,
    async_operation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let query_exists = sqlx::query_scalar::<_, Uuid>(
        "select id
         from query_execution
         where id = $1
         for update",
    )
    .bind(execution_id)
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    if !query_exists {
        transaction.commit().await?;
        return Ok(false);
    }

    let runtime_state = sqlx::query_scalar::<_, String>(
        "select lifecycle_state::text
         from runtime_execution
         where id = $1
           and owner_kind = 'query_execution'
           and owner_id = $2
         for update",
    )
    .bind(runtime_execution_id)
    .bind(execution_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if !matches!(runtime_state.as_deref(), Some("accepted" | "running")) {
        transaction.commit().await?;
        return Ok(false);
    }

    let completed_at = Utc::now();
    sqlx::query(
        "update runtime_execution
         set lifecycle_state = 'canceled',
             active_stage = null,
             failure_code = $3,
             failure_summary_redacted = 'query execution interrupted before terminal persistence',
             completed_at = $4
         where id = $1
           and owner_id = $2
           and lifecycle_state in ('accepted', 'running')",
    )
    .bind(runtime_execution_id)
    .bind(execution_id)
    .bind(INTERRUPTED_QUERY_EXECUTION_FAILURE_CODE)
    .bind(completed_at)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update ops_async_operation
         set status = 'canceled',
             completed_at = $4,
             failure_code = $5
         where id = $1
           and subject_kind = 'query_execution'
           and subject_id = $2
           and operation_kind = $3
           and status in ('accepted', 'processing')",
    )
    .bind(async_operation_id)
    .bind(execution_id)
    .bind("query_execution")
    .bind(completed_at)
    .bind(INTERRUPTED_QUERY_EXECUTION_FAILURE_CODE)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(true)
}

pub async fn reap_stale_query_executions(
    postgres: &PgPool,
    stale_before: DateTime<Utc>,
    batch_limit: i64,
) -> Result<u64, sqlx::Error> {
    let candidates = sqlx::query_as::<_, StaleQueryExecutionCandidate>(
        "select
            query.id as execution_id,
            query.runtime_execution_id,
            operation.id as async_operation_id
         from query_execution query
         join runtime_execution runtime
           on runtime.id = query.runtime_execution_id
          and runtime.owner_kind = 'query_execution'
          and runtime.owner_id = query.id
          and runtime.lifecycle_state in ('accepted', 'running')
         join lateral (
            select candidate.id
            from ops_async_operation candidate
            where candidate.subject_kind = 'query_execution'
              and candidate.subject_id = query.id
              and candidate.operation_kind = 'query_execution'
              and candidate.status in ('accepted', 'processing')
            order by candidate.created_at desc, candidate.id desc
            limit 1
         ) operation on true
         where runtime.accepted_at < $1
         order by runtime.accepted_at, query.id
         limit $2",
    )
    .bind(stale_before)
    .bind(batch_limit.max(1))
    .fetch_all(postgres)
    .await?;

    let mut reaped = 0u64;
    for candidate in candidates {
        if cancel_interrupted_execution(
            postgres,
            candidate.execution_id,
            candidate.runtime_execution_id,
            candidate.async_operation_id,
        )
        .await?
        {
            reaped += 1;
        }
    }
    Ok(reaped)
}

/// Bounded, idempotent sweep of `query_execution` async operations whose linked
/// runtime execution is already terminal (or whose execution/runtime row is
/// absent).
///
/// [`reap_stale_query_executions`] only selects operations whose runtime is
/// still `accepted` / `running`, so an operation whose runtime already reached
/// a terminal state — or whose `query_execution` / `runtime_execution` row
/// never appeared — was stranded in `processing` forever. This sweep closes
/// that residual: it maps the terminal runtime state to the operation's own
/// terminal status (`ready` for `completed` / `recovered`, `canceled` for
/// `canceled`, otherwise `failed`), stamps `completed_at` and records
/// `reconciled_orphaned_attempt` on every non-`ready` row.
///
/// Only `operation_kind = 'query_execution'` rows are selected, matching
/// [`reap_stale_query_executions`]. The selection is capped at `batch_limit`
/// rows ordered by `created_at, id` and the
/// `status IN ('accepted', 'processing')` guard makes a repeated run over the
/// same backlog report zero.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the operations.
pub async fn reconcile_orphan_query_operations_once(
    postgres: &PgPool,
    stale_before: DateTime<Utc>,
    batch_limit: i64,
) -> Result<u64, sqlx::Error> {
    let reconciled = sqlx::query_scalar::<_, Uuid>(
        "with targets as materialized (
             select
                 operation.id as operation_id,
                 runtime.lifecycle_state::text as runtime_lifecycle_state
             from ops_async_operation as operation
             left join query_execution as query
               on query.id = operation.subject_id
             left join runtime_execution as runtime
               on runtime.id = query.runtime_execution_id
              and runtime.owner_kind = 'query_execution'
              and runtime.owner_id = query.id
             where operation.subject_kind = 'query_execution'
               and operation.operation_kind = 'query_execution'
               and operation.status in ('accepted', 'processing')
               and operation.created_at < $1
               and (
                   query.id is null
                   or runtime.id is null
                   or runtime.lifecycle_state in ('completed', 'recovered', 'failed', 'canceled')
               )
             order by operation.created_at, operation.id
             limit $2
         )
         update ops_async_operation as operation
         set status = case
                 when targets.runtime_lifecycle_state in ('completed', 'recovered') then 'ready'
                 when targets.runtime_lifecycle_state = 'canceled' then 'canceled'
                 else 'failed'
             end::ops_async_operation_status,
             completed_at = now(),
             failure_code = case
                 when targets.runtime_lifecycle_state in ('completed', 'recovered') then null
                 else 'reconciled_orphaned_attempt'
             end
         from targets
         where operation.id = targets.operation_id
           and operation.status in ('accepted', 'processing')
         returning operation.id",
    )
    .bind(stale_before)
    .bind(batch_limit.max(1))
    .fetch_all(postgres)
    .await?;
    Ok(u64::try_from(reconciled.len()).unwrap_or(u64::MAX))
}
