use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewOpsAsyncOperation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: &'a str,
    pub surface_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub status: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: Option<Uuid>,
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
    pub readable_document_count: i64,
    pub failed_document_count: i64,
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
            created_at,
            completed_at,
            failure_code
         from ops_async_operation
         where id = $1",
    )
    .bind(operation_id)
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
            created_at,
            completed_at,
            failure_code
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
            created_at,
            completed_at,
            failure_code
         from ops_async_operation
         where subject_kind = $1
           and subject_id = $2
         order by created_at desc, id desc
         limit 1",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_async_operation(
    postgres: &PgPool,
    input: &NewOpsAsyncOperation<'_>,
) -> Result<OpsAsyncOperationRow, sqlx::Error> {
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
            created_at,
            completed_at,
            failure_code
        )
        values ($1, $2, $3, $4, $5::surface_kind, $6, $7::ops_async_operation_status, $8, $9, now(), $10, $11)
        returning
            id,
            workspace_id,
            library_id,
            operation_kind,
            surface_kind::text as surface_kind,
            status::text as status,
            subject_kind,
            subject_id,
            created_at,
            completed_at,
            failure_code",
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
    .bind(input.completed_at)
    .bind(input.failure_code)
    .fetch_one(postgres)
    .await
}

pub async fn update_async_operation(
    postgres: &PgPool,
    operation_id: Uuid,
    input: &UpdateOpsAsyncOperation<'_>,
) -> Result<Option<OpsAsyncOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsAsyncOperationRow>(
        "update ops_async_operation
         set status = $2::ops_async_operation_status,
             completed_at = $3,
             failure_code = $4
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
            created_at,
            completed_at,
            failure_code",
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
            0::bigint as readable_document_count,
            0::bigint as failed_document_count,
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

pub async fn list_recent_failed_ingest_attempts(
    postgres: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<OpsLibraryFailureRow>, sqlx::Error> {
    sqlx::query_as::<_, OpsLibraryFailureRow>(
        "select
            coalesce(attempt.finished_at, attempt.heartbeat_at, attempt.started_at) as created_at,
            attempt.failure_code
         from ingest_attempt attempt
         join ingest_job job on job.id = attempt.job_id
         where job.library_id = $1
           and attempt.attempt_state = 'failed'
         order by coalesce(attempt.finished_at, attempt.heartbeat_at, attempt.started_at) desc, attempt.id desc
         limit $2",
    )
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
            coalesce(runtime.completed_at, execution.completed_at, execution.started_at) as created_at,
            coalesce(runtime.failure_code, execution.failure_code) as failure_code
         from query_execution execution
         join runtime_execution runtime on runtime.id = execution.runtime_execution_id
         where execution.library_id = $1
           and runtime.lifecycle_state = 'failed'
           and coalesce(runtime.failure_code, execution.failure_code) is not null
           and coalesce(runtime.failure_code, execution.failure_code) ilike '%context bundle%'
         order by coalesce(runtime.completed_at, execution.completed_at, execution.started_at) desc, execution.id desc
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}
