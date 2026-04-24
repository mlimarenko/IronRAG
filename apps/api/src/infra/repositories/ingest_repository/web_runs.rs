use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct WebIngestRunRow {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub async_operation_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub seed_url: String,
    pub normalized_seed_url: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub ignore_patterns: Value,
    pub run_state: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewWebIngestRun<'a> {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub async_operation_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: &'a str,
    pub seed_url: &'a str,
    pub normalized_seed_url: &'a str,
    pub boundary_policy: &'a str,
    pub max_depth: i32,
    pub max_pages: i32,
    pub ignore_patterns: Value,
    pub run_state: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub requested_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdateWebIngestRun<'a> {
    pub run_state: &'a str,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WebRunCountsRow {
    pub discovered: i64,
    pub eligible: i64,
    pub processed: i64,
    pub queued: i64,
    pub processing: i64,
    pub duplicates: i64,
    pub excluded: i64,
    pub blocked: i64,
    pub failed: i64,
    pub canceled: i64,
    pub last_activity_at: Option<DateTime<Utc>>,
}

pub async fn create_web_ingest_run(
    postgres: &PgPool,
    input: &NewWebIngestRun<'_>,
) -> Result<WebIngestRunRow, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "insert into content_web_ingest_run (
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode,
            seed_url,
            normalized_seed_url,
            boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6::web_ingest_mode,
            $7,
            $8,
            $9::web_boundary_policy,
            $10,
            $11,
            $12,
            $13::web_run_state,
            $14,
            coalesce($15, now()),
            $16,
            $17,
            $18
        )
        returning
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at",
    )
    .bind(input.id)
    .bind(input.mutation_id)
    .bind(input.async_operation_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mode)
    .bind(input.seed_url)
    .bind(input.normalized_seed_url)
    .bind(input.boundary_policy)
    .bind(input.max_depth)
    .bind(input.max_pages)
    .bind(&input.ignore_patterns)
    .bind(input.run_state)
    .bind(input.requested_by_principal_id)
    .bind(input.requested_at)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .bind(input.cancel_requested_at)
    .fetch_one(postgres)
    .await
}

pub async fn get_web_ingest_run_by_id(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where id = $1",
    )
    .bind(run_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_ingest_run_by_mutation_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where mutation_id = $1",
    )
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_web_ingest_runs(
    postgres: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where library_id = $1
         order by requested_at desc, id desc
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Clone, FromRow)]
pub struct WebRunCountsByRunRow {
    pub run_id: Uuid,
    pub discovered: i64,
    pub eligible: i64,
    pub processed: i64,
    pub queued: i64,
    pub processing: i64,
    pub duplicates: i64,
    pub excluded: i64,
    pub blocked: i64,
    pub failed: i64,
    pub canceled: i64,
    pub last_activity_at: Option<DateTime<Utc>>,
}

/// Batched version of [`get_web_run_counts`] — returns one row per
/// requested `run_id` in a single indexed aggregation. Callers that
/// render a list of runs MUST use this helper; the per-id variant in a
/// loop is an N+1 hazard that on reference-sized libraries pushes the
/// web-runs endpoint past the browser timeout.
pub async fn list_web_run_counts_by_run_ids(
    postgres: &PgPool,
    run_ids: &[Uuid],
) -> Result<Vec<WebRunCountsByRunRow>, sqlx::Error> {
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, WebRunCountsByRunRow>(
        "select
            run_id,
            count(*)::bigint as discovered,
            count(*) filter (
                where candidate_state in ('eligible', 'queued', 'processing', 'processed', 'failed', 'canceled')
            )::bigint as eligible,
            count(*) filter (where candidate_state = 'processed')::bigint as processed,
            count(*) filter (where candidate_state = 'queued')::bigint as queued,
            count(*) filter (where candidate_state = 'processing')::bigint as processing,
            count(*) filter (where candidate_state = 'duplicate')::bigint as duplicates,
            count(*) filter (where candidate_state = 'excluded')::bigint as excluded,
            count(*) filter (where candidate_state = 'blocked')::bigint as blocked,
            count(*) filter (where candidate_state = 'failed')::bigint as failed,
            count(*) filter (where candidate_state = 'canceled')::bigint as canceled,
            max(updated_at) as last_activity_at
         from content_web_discovered_page
         where run_id = any($1)
         group by run_id",
    )
    .bind(run_ids)
    .fetch_all(postgres)
    .await
}

pub async fn update_web_ingest_run(
    postgres: &PgPool,
    run_id: Uuid,
    input: &UpdateWebIngestRun<'_>,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "update content_web_ingest_run
         set run_state = $2::web_run_state,
             completed_at = $3,
             failure_code = $4,
             cancel_requested_at = $5
         where id = $1
         returning
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            ignore_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at",
    )
    .bind(run_id)
    .bind(input.run_state)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .bind(input.cancel_requested_at)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_run_counts(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<WebRunCountsRow, sqlx::Error> {
    sqlx::query_as::<_, WebRunCountsRow>(
        "select
            count(*)::bigint as discovered,
            count(*) filter (
                where candidate_state in ('eligible', 'queued', 'processing', 'processed', 'failed', 'canceled')
            )::bigint as eligible,
            count(*) filter (where candidate_state = 'processed')::bigint as processed,
            count(*) filter (where candidate_state = 'queued')::bigint as queued,
            count(*) filter (where candidate_state = 'processing')::bigint as processing,
            count(*) filter (where candidate_state = 'duplicate')::bigint as duplicates,
            count(*) filter (where candidate_state = 'excluded')::bigint as excluded,
            count(*) filter (where candidate_state = 'blocked')::bigint as blocked,
            count(*) filter (where candidate_state = 'failed')::bigint as failed,
            count(*) filter (where candidate_state = 'canceled')::bigint as canceled,
            max(updated_at) as last_activity_at
         from content_web_discovered_page
         where run_id = $1",
    )
    .bind(run_id)
    .fetch_one(postgres)
    .await
}
