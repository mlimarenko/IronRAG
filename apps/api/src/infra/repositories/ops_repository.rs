use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgConnection, PgPool, Postgres};
use uuid::Uuid;

use super::content_document_list_repository::{
    CURRENT_DOCUMENT_REVISION_JOIN_SQL, DERIVED_STATUS_CASE_SQL, LATEST_DOCUMENT_INGEST_JOB_SQL,
};

#[derive(Debug, Clone, FromRow)]
pub struct OpsAsyncOperationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub operation_kind: String,
    pub surface_kind: String,
    pub status: String,
    pub subject_kind: String,
    pub subject_id: Option<Uuid>,
    pub parent_async_operation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    /// Server-authoritative terminal disposition, written only by the ingest
    /// failure finalisation. `None` for older rows, unsettled rows, and every
    /// operation kind whose failure path is out of scope.
    pub failure_disposition: Option<String>,
    /// Terminal per-kind result payload, written only by the guarded settle in
    /// `settle_async_operation_with_result`. `None` for every operation kind
    /// that publishes no result, and for every row that has not settled yet.
    pub result_json: Option<serde_json::Value>,
}

/// Aggregated child-operation progress for a parent `ops_async_operation`.
///
/// Populated via a single grouped count query over `parent_async_operation_id`
/// — the primary progress source for every batch-ops endpoint.
#[derive(Debug, Clone, FromRow)]
pub struct OpsAsyncOperationProgressRow {
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub in_flight: i64,
}

#[derive(Debug, Clone)]
pub struct NewOpsAsyncOperation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub operation_kind: &'a str,
    pub surface_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub status: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: Option<Uuid>,
    pub parent_async_operation_id: Option<Uuid>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct UpdateOpsAsyncOperation<'a> {
    pub status: &'a str,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OpsLibraryFactsRow {
    pub library_id: Uuid,
    pub queue_depth: i64,
    pub running_attempts: i64,
    pub degraded_state: String,
    pub last_recomputed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OpsLibraryWarningRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub warning_kind: String,
    pub severity: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OpsLibraryFailureRow {
    pub created_at: DateTime<Utc>,
    pub failure_code: Option<String>,
}

pub async fn get_async_operation_by_id(
    postgres: &PgPool,
    operation_id: Uuid,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json
         from ops_async_operation
         where id = $1",
    )
    .bind(operation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_provisioning_deletion_operation(
    postgres: &PgPool,
    operation_id: Uuid,
    requested_by_principal_id: Uuid,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "select
            id, workspace_id, library_id, operation_kind, surface_kind::text as surface_kind,
            status::text as status, subject_kind, subject_id, parent_async_operation_id,
            created_at, completed_at, failure_code, failure_disposition, result_json
         from ops_async_operation
         where id = $1
           and requested_by_principal_id = $2
           and operation_kind in ('delete_workspace', 'delete_library')
           and surface_kind = 'rest'",
    )
    .bind(operation_id)
    .bind(requested_by_principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_async_operations_by_ids(
    postgres: &PgPool,
    operation_ids: &[Uuid],
) -> Result<Vec<OpsAsyncOperationRow>, sqlx::Error> {
    if operation_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json
         from ops_async_operation
         where id = any($1)",
    )
    .bind(operation_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_latest_async_operation_by_subject(
    postgres: &PgPool,
    subject_kind: &str,
    subject_id: Uuid,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    get_latest_async_operation_by_subject_on(postgres, subject_kind, subject_id).await
}

pub async fn get_latest_async_operation_by_subject_with_transaction(
    transaction: &mut PgConnection,
    subject_kind: &str,
    subject_id: Uuid,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    get_latest_async_operation_by_subject_on(&mut *transaction, subject_kind, subject_id).await
}

/// Actor, subject and resource scope of an existing provisioning deletion.
pub struct ProvisioningDeletionReplayQuery<'a> {
    pub actor_principal_id: Uuid,
    pub operation_kind: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
}

pub async fn get_provisioning_deletion_replay_with_transaction(
    transaction: &mut PgConnection,
    input: ProvisioningDeletionReplayQuery<'_>,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    let ProvisioningDeletionReplayQuery {
        actor_principal_id,
        operation_kind,
        subject_kind,
        subject_id,
        workspace_id,
        library_id,
    } = input;
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "select
            id, workspace_id, library_id, operation_kind, surface_kind::text as surface_kind,
            status::text as status, subject_kind,
            subject_id, parent_async_operation_id, created_at,
            completed_at, failure_code, failure_disposition, result_json
         from ops_async_operation
         where requested_by_principal_id = $1
           and operation_kind = $2
           and subject_kind = $3
           and subject_id = $4
           and workspace_id = $5
           and (
             ($6::uuid is null and library_id is null)
             or ($6::uuid is not null and (library_id = $6 or library_id is null))
           )
         order by created_at desc, id desc
         limit 1
         for update",
    )
    .bind(actor_principal_id)
    .bind(operation_kind)
    .bind(subject_kind)
    .bind(subject_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await
    .map(|operation| {
        operation.filter(|operation| {
            matches!(operation.status.as_str(), "accepted" | "processing" | "ready")
        })
    })
}

async fn get_latest_async_operation_by_subject_on<'e, E>(
    executor: E,
    subject_kind: &str,
    subject_id: Uuid,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json
         from ops_async_operation
         where subject_kind = $1
           and subject_id = $2
         order by created_at desc, id desc
         limit 1",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(executor)
    .await
}

pub async fn create_async_operation(
    postgres: &PgPool,
    input: &NewOpsAsyncOperation<'_>,
) -> Result<OpsAsyncOperationRow, sqlx::Error> {
    create_async_operation_on(postgres, input).await
}

pub async fn create_async_operation_with_transaction(
    transaction: &mut PgConnection,
    input: &NewOpsAsyncOperation<'_>,
) -> Result<OpsAsyncOperationRow, sqlx::Error> {
    create_async_operation_on(&mut *transaction, input).await
}

async fn create_async_operation_on<'e, E>(
    executor: E,
    input: &NewOpsAsyncOperation<'_>,
) -> Result<OpsAsyncOperationRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "insert into ops_async_operation (
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind,
            requested_by_principal_id,
            status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code
        )
        values ($1, $2, $3, $4, $5::surface_kind, $6, $7::ops_async_operation_status, $8, $9, $10, now(), $11, $12)
        returning
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.operation_kind)
    .bind(input.surface_kind)
    .bind(input.requested_by_principal_id)
    .bind(input.status)
    .bind(input.subject_kind)
    .bind(input.subject_id)
    .bind(input.parent_async_operation_id)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .fetch_one(executor)
    .await
}

pub async fn get_async_operation_progress(
    postgres: &PgPool,
    parent_id: Uuid,
) -> Result<OpsAsyncOperationProgressRow, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationProgressRow>(
        "select
            count(*)::bigint as total,
            count(*) filter (where status = 'ready')::bigint as completed,
            count(*) filter (where status in ('failed', 'canceled', 'superseded'))::bigint as failed,
            count(*) filter (where status in ('accepted', 'processing'))::bigint as in_flight
         from ops_async_operation
         where parent_async_operation_id = $1",
    )
    .bind(parent_id)
    .fetch_one(postgres)
    .await
}

/// Which persisted parent statuses a fan-out settle may transition from.
///
/// The settle never reopens a row it was not told to: the live worker, its
/// abnormal-exit guard and the stranded-parent repair all use [`InFlight`];
/// only the operator repair of a parent whose recorded failure contradicts its
/// children uses [`ContradictedFailure`], and even then the settle fires only
/// when the children disagree with the recorded failure.
///
/// [`InFlight`]: FanOutSettleGuard::InFlight
/// [`ContradictedFailure`]: FanOutSettleGuard::ContradictedFailure
#[derive(Debug, Clone, Copy)]
pub enum FanOutSettleGuard {
    /// A parent still `accepted`/`processing`.
    InFlight,
    /// A parent already terminal `failed` whose recorded outcome contradicts
    /// its persisted children (every child `ready`). The settle re-derives the
    /// terminal state from the children, so a genuine failure — one whose
    /// children really failed — is never reopened.
    ContradictedFailure,
}

impl FanOutSettleGuard {
    const fn predicate(self) -> &'static str {
        match self {
            Self::InFlight => "parent.status in ('accepted', 'processing')",
            Self::ContradictedFailure => "parent.status = 'failed' and derived.status = 'ready'",
        }
    }
}

/// The single point of truth for a fan-out parent's terminal state, derived
/// from its persisted children in one statement.
///
/// A batch endpoint creates one parent `ops_async_operation` and fans out to
/// child operations linked by `parent_async_operation_id`; the parent is the
/// only row a poller watches. Every writer of a batch parent's status — the
/// live worker, its abnormal-exit guard, the operator repair — goes through
/// this statement, which derives the terminal state as a pure function of the
/// children:
///
/// * every child terminal → `ready`, or `failed` with
///   `children_failed:<failed>/<total>` when at least one child is
///   `failed`/`canceled`/`superseded`;
/// * a child still in `accepted`/`processing`, plus `fallback_failure_code` →
///   `failed` with that code. A caller passes a code only when the children
///   cannot decide and nothing else will settle the parent (the detached worker
///   was aborted, the fan-out timed out, or every selected document failed
///   before a child row existed);
/// * no child at all (the fan-out's items are not child `ops_async_operation`
///   rows) and `childless_status` set → that status. The caller's observed
///   fan-out outcome is the aggregate input when there is nothing to aggregate;
/// * otherwise no row matches and the parent is left untouched.
///
/// A failing *background* step (graph evidence cleanup, graph projection
/// refresh) is not a deletion failure and must never be written here as the
/// parent's status: the caller records it in `detail`, merged into the parent's
/// `result_json` while the children still decide the status. `detail` is
/// `null` for callers that have nothing to attach.
///
/// The update is guarded by [`guard`], so a concurrent settle wins exactly
/// once. A rewrite that does not end `failed` clears `failure_disposition`;
/// only a transition to `failed` keeps the earlier terminal proof.
///
/// [`guard`]: FanOutSettleGuard
pub async fn settle_fan_out_operation_from_children(
    postgres: &PgPool,
    parent_id: Uuid,
    fallback_failure_code: Option<&str>,
    childless_status: Option<&str>,
    detail: Option<&serde_json::Value>,
    guard: FanOutSettleGuard,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    let sql = format!(
        "with children as (
             select count(*)::bigint as total,
                    count(*) filter (where status in ('accepted', 'processing'))::bigint as in_flight,
                    count(*) filter (where status in ('failed', 'canceled', 'superseded'))::bigint as failed
               from ops_async_operation
              where parent_async_operation_id = $1
         ),
         derived as (
             select
                 case
                     when children.total > 0 and children.in_flight = 0 then
                         case when children.failed > 0 then 'failed' else 'ready' end
                     when $2::text is not null then 'failed'
                     when children.total = 0 and $3::text is not null then $3::text
                     else null
                 end as status,
                 case
                     when children.total > 0 and children.in_flight = 0 then
                         case
                             when children.failed > 0
                                 then format('children_failed:%s/%s', children.failed, children.total)
                             else null
                         end
                     else $2::text
                 end as failure_code
               from children
         )
         update ops_async_operation parent
            set status = derived.status::ops_async_operation_status,
                completed_at = now(),
                failure_code = derived.failure_code,
                failure_disposition = case
                    when derived.status = 'failed' then parent.failure_disposition
                    else null
                end,
                result_json = case
                    when $4::jsonb is null then parent.result_json
                    else coalesce(parent.result_json, '{{}}'::jsonb) || $4::jsonb
                end
           from derived
          where parent.id = $1
            and {}
            and derived.status is not null
         returning
            parent.id,
            parent.workspace_id,
            parent.library_id,
            parent.operation_kind,
            parent.surface_kind::text as surface_kind,
            parent.status::text as status,
            parent.subject_kind,
            parent.subject_id,
            parent.parent_async_operation_id,
            parent.created_at,
            parent.completed_at,
            parent.failure_code,
            parent.failure_disposition,
            parent.result_json",
        guard.predicate()
    );
    sqlx::query_as::<_, OpsAsyncOperationRow>(sqlx::AssertSqlSafe(sql))
        .bind(parent_id)
        .bind(fallback_failure_code)
        .bind(childless_status)
        .bind(detail)
        .fetch_optional(postgres)
        .await
}

/// Settles an operation and publishes its terminal result in one statement.
///
/// Separate from `update_async_operation` on purpose, and guarded on purpose.
/// The generic update writes `where id = $1` with no status predicate, which is
/// correct for the callers that have it: a batch parent is written more than
/// once as its children settle, and an ingest retry deliberately moves a
/// settled row back to `processing`. A result payload cannot ride on that —
/// the second write of a batch parent would erase the payload the first one
/// published, and a retry would leave the previous run's counters attached to a
/// row that is running again. Restricting the write to a row that has not
/// settled yet makes the result write-once: the first terminal transition wins
/// and later writes match no row, so `None` here means "already settled",
/// never "lost".
///
/// A row that is no longer settling to `failed` carries no terminal proof, so
/// the write clears `failure_disposition` in the same statement. A reopened or
/// re-settled row must not serve the earlier failure's disposition.
pub async fn settle_async_operation_with_result(
    postgres: &PgPool,
    operation_id: Uuid,
    status: &str,
    completed_at: Option<DateTime<Utc>>,
    failure_code: Option<&str>,
    result_json: Option<&serde_json::Value>,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "update ops_async_operation
         set status = $2::ops_async_operation_status,
             completed_at = $3,
             failure_code = $4,
             failure_disposition = case
                 when $2::ops_async_operation_status = 'failed' then failure_disposition
                 else null
             end,
             result_json = $5
         where id = $1 and status in ('accepted', 'processing')
         returning
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json",
    )
    .bind(operation_id)
    .bind(status)
    .bind(completed_at)
    .bind(failure_code)
    .bind(result_json)
    .fetch_optional(postgres)
    .await
}

/// Moves an operation to a new status without touching its result payload.
///
/// Used for the batch-parent rewrites and the ingest retry that deliberately
/// reopens a settled row. Only a write that settles to `failed` keeps the
/// stored `failure_disposition`; every other transition clears it, so a row
/// moved to `ready` or back to `processing` cannot keep serving the earlier
/// failure's terminal proof.
pub async fn update_async_operation(
    postgres: &PgPool,
    operation_id: Uuid,
    input: &UpdateOpsAsyncOperation<'_>,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "update ops_async_operation
         set status = $2::ops_async_operation_status,
             completed_at = $3,
             failure_code = $4,
             failure_disposition = case
                 when $2::ops_async_operation_status = 'failed' then failure_disposition
                 else null
             end
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            parent_async_operation_id,
            created_at,
            completed_at,
            failure_code,
            failure_disposition,
            result_json",
    )
    .bind(operation_id)
    .bind(input.status)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .fetch_optional(postgres)
    .await
}

pub async fn get_library_facts(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<OpsLibraryFactsRow>, sqlx::Error> {
    // Queue-health-only aggregate. Document-level counts are NOT part
    // of this row — they live in `aggregate_library_document_metrics`
    // as the single source of truth, and `map_library_facts_from_aggregate`
    // combines both aggregates into the service-layer `OpsLibraryState`.
    sqlx::query_as::<_, OpsLibraryFactsRow>(
        "select
            $1 as library_id,
            (
                select count(*)::bigint
                from ingest_job job
                where job.library_id = $1
                  and job.queue_state = 'queued'
            ) as queue_depth,
            (
                select count(*)::bigint
                from ingest_attempt attempt
                join ingest_job job on job.id = attempt.job_id
                where job.library_id = $1
                  and attempt.attempt_state in ('leased', 'running')
            ) as running_attempts,
            'healthy'::text as degraded_state,
            now() as last_recomputed_at",
    )
    .bind(library_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_library_warnings(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<OpsLibraryWarningRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsLibraryWarningRow>(
        "select id, library_id, warning_kind, severity, created_at, resolved_at
         from ops_library_warning
         where library_id = $1
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

/// When the oldest claimable job in this library was queued, or `None` when
/// nothing is waiting.
///
/// This is the only signal that separates "documents are being ingested" from
/// "documents are stuck in the queue". It is read inside the dashboard
/// snapshot's repeatable-read transaction (`load_library_dashboard_snapshot`)
/// so a coverage notice cannot combine it with counts observed at a different
/// MVCC instant.
pub async fn oldest_available_queued_job_at_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "select min(queued_at)
         from ingest_job
         where library_id = $1
           and queue_state = 'queued'
           and available_at <= now()",
    )
    .bind(library_id)
    .fetch_one(executor)
    .await
}

/// Recent failed ingest attempts that still need an operator: an attempt failed
/// and its document is still derived `failed` on the documents screen.
///
/// The notice routes to `/documents?status=failed`, so it may fire only while
/// that bucket is non-empty. A failure that a later queued/leased/completed
/// retry supersedes, a background refresh over a still-readable revision, and a
/// failure whose document link is gone all leave the document out of the failed
/// bucket; announcing them sent the operator to an empty list. The derived-status
/// rule is the same `DERIVED_STATUS_CASE_SQL` the list and the metrics use, so
/// the notice and the screen cannot disagree.
pub async fn list_recent_failed_ingest_attempts(
    postgres: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<OpsLibraryFailureRow>, sqlx::Error> {
    let sql = format!(
        "select
            coalesce(attempt.finished_at, attempt.heartbeat_at, attempt.started_at) as created_at,
            attempt.failure_code
         from ingest_attempt attempt
         join ingest_job job on job.id = attempt.job_id
         join content_document d
             on d.id = job.knowledge_document_id
            and d.document_state = 'active'
            and d.deleted_at is null
          {CURRENT_DOCUMENT_REVISION_JOIN_SQL}
          left join content_document_head h on h.document_id = d.id
         left join content_mutation m on m.id = h.latest_mutation_id
         left join lateral (
             {LATEST_DOCUMENT_INGEST_JOB_SQL}
         ) ij on true
         where job.library_id = $1
           and job.queue_state = 'failed'
           and attempt.attempt_state = 'failed'
           and ({DERIVED_STATUS_CASE_SQL}) = 'failed'
         order by coalesce(attempt.finished_at, attempt.heartbeat_at, attempt.started_at) desc, attempt.id desc
         limit $2"
    );
    sqlx::query_as::<_, OpsLibraryFailureRow>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .bind(limit)
        .fetch_all(postgres)
        .await
}

pub async fn list_recent_bundle_assembly_failures(
    postgres: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<OpsLibraryFailureRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsLibraryFailureRow>(
        "select
            runtime.completed_at as created_at,
            coalesce(runtime.failure_code, execution.failure_code) as failure_code
         from query_execution execution
         join runtime_execution runtime
           on runtime.id = execution.runtime_execution_id
          and runtime.owner_kind = 'query_execution'
          and runtime.owner_id = execution.id
         where execution.library_id = $1
           and runtime.lifecycle_state = 'failed'
           and runtime.completed_at is not null
           and coalesce(runtime.failure_code, execution.failure_code) is not null
           and coalesce(runtime.failure_code, execution.failure_code) ilike '%context bundle%'
         order by runtime.completed_at desc, runtime.id desc
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}
