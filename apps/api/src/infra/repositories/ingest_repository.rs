use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct IngestJobRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewIngestJob {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub queued_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdateIngestJob {
    pub mutation_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IngestAttemptRow {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt_number: i32,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone)]
pub struct NewIngestAttempt {
    pub job_id: Uuid,
    pub attempt_number: i32,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateIngestAttempt {
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct IngestStageEventRow {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub ordinal: i32,
    pub message: Option<String>,
    pub details_json: Value,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewIngestStageEvent {
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub ordinal: i32,
    pub message: Option<String>,
    pub details_json: Value,
    pub recorded_at: Option<DateTime<Utc>>,
}

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
pub struct WebDiscoveredPageRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub discovered_url: Option<String>,
    pub normalized_url: String,
    pub final_url: Option<String>,
    pub canonical_url: Option<String>,
    pub depth: i32,
    pub referrer_candidate_id: Option<Uuid>,
    pub host_classification: String,
    pub candidate_state: String,
    pub classification_reason: Option<String>,
    pub content_type: Option<String>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<String>,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewWebDiscoveredPage<'a> {
    pub id: Uuid,
    pub run_id: Uuid,
    pub discovered_url: Option<&'a str>,
    pub normalized_url: &'a str,
    pub final_url: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub depth: i32,
    pub referrer_candidate_id: Option<Uuid>,
    pub host_classification: &'a str,
    pub candidate_state: &'a str,
    pub classification_reason: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<&'a str>,
    pub discovered_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateWebDiscoveredPage<'a> {
    pub final_url: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub host_classification: Option<&'a str>,
    pub candidate_state: &'a str,
    pub classification_reason: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<&'a str>,
    pub updated_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
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

pub async fn create_ingest_job(
    postgres: &PgPool,
    input: &NewIngestJob,
) -> Result<IngestJobRow, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "insert into ingest_job (
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind,
            queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9::ingest_job_kind,
            $10::ingest_queue_state,
            $11,
            $12,
            coalesce($13, now()),
            coalesce($14, now()),
            $15
        )
        returning
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.connector_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(&input.job_kind)
    .bind(&input.queue_state)
    .bind(input.priority)
    .bind(&input.dedupe_key)
    .bind(input.queued_at)
    .bind(input.available_at)
    .bind(input.completed_at)
    .fetch_one(postgres)
    .await
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
            $12::web_run_state,
            $13,
            coalesce($14, now()),
            $15,
            $16,
            $17
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
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where library_id = $1
         order by requested_at desc, id desc",
    )
    .bind(library_id)
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

pub async fn create_web_discovered_page(
    postgres: &PgPool,
    input: &NewWebDiscoveredPage<'_>,
) -> Result<WebDiscoveredPageRow, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "insert into content_web_discovered_page (
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification,
            candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9::web_candidate_host_classification,
            $10::web_candidate_state,
            $11,
            $12,
            $13,
            $14,
            coalesce($15, now()),
            coalesce($16, now()),
            $17,
            $18,
            $19
        )
        returning
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id",
    )
    .bind(input.id)
    .bind(input.run_id)
    .bind(input.discovered_url)
    .bind(input.normalized_url)
    .bind(input.final_url)
    .bind(input.canonical_url)
    .bind(input.depth)
    .bind(input.referrer_candidate_id)
    .bind(input.host_classification)
    .bind(input.candidate_state)
    .bind(input.classification_reason)
    .bind(input.content_type)
    .bind(input.http_status)
    .bind(input.snapshot_storage_key)
    .bind(input.discovered_at)
    .bind(input.updated_at)
    .bind(input.document_id)
    .bind(input.result_revision_id)
    .bind(input.mutation_item_id)
    .fetch_one(postgres)
    .await
}

pub async fn list_web_discovered_pages(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<Vec<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where run_id = $1
         order by depth asc, discovered_at asc, id asc",
    )
    .bind(run_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_web_discovered_page_by_result_revision_id(
    postgres: &PgPool,
    result_revision_id: Uuid,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where result_revision_id = $1
         order by updated_at desc, discovered_at desc, id desc
         limit 1",
    )
    .bind(result_revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_web_discovered_pages_by_result_revision_ids(
    postgres: &PgPool,
    result_revision_ids: &[Uuid],
) -> Result<Vec<WebDiscoveredPageRow>, sqlx::Error> {
    if result_revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select distinct on (result_revision_id)
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where result_revision_id = any($1)
         order by result_revision_id, updated_at desc, discovered_at desc, id desc",
    )
    .bind(result_revision_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_web_discovered_page_by_id(
    postgres: &PgPool,
    candidate_id: Uuid,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where id = $1",
    )
    .bind(candidate_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_discovered_page_by_run_and_normalized_url(
    postgres: &PgPool,
    run_id: Uuid,
    normalized_url: &str,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where run_id = $1
           and normalized_url = $2
         limit 1",
    )
    .bind(run_id)
    .bind(normalized_url)
    .fetch_optional(postgres)
    .await
}

pub async fn update_web_discovered_page(
    postgres: &PgPool,
    candidate_id: Uuid,
    input: &UpdateWebDiscoveredPage<'_>,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "update content_web_discovered_page
         set final_url = $2,
             canonical_url = $3,
             host_classification = coalesce($4::web_candidate_host_classification, host_classification),
             candidate_state = $5::web_candidate_state,
             classification_reason = $6,
             content_type = $7,
             http_status = $8,
             snapshot_storage_key = $9,
             updated_at = coalesce($10, now()),
             document_id = $11,
             result_revision_id = $12,
             mutation_item_id = $13
         where id = $1
         returning
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id",
    )
    .bind(candidate_id)
    .bind(input.final_url)
    .bind(input.canonical_url)
    .bind(input.host_classification)
    .bind(input.candidate_state)
    .bind(input.classification_reason)
    .bind(input.content_type)
    .bind(input.http_status)
    .bind(input.snapshot_storage_key)
    .bind(input.updated_at)
    .bind(input.document_id)
    .bind(input.result_revision_id)
    .bind(input.mutation_item_id)
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

pub async fn get_ingest_job_by_id(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where id = $1",
    )
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_ingest_job_by_dedupe_key(
    postgres: &PgPool,
    library_id: Uuid,
    dedupe_key: &str,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where library_id = $1
           and dedupe_key = $2
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(library_id)
    .bind(dedupe_key)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_mutation_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where mutation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_async_operation_id(
    postgres: &PgPool,
    async_operation_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where async_operation_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(async_operation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_latest_ingest_job_by_knowledge_revision_id(
    postgres: &PgPool,
    knowledge_revision_id: Uuid,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where knowledge_revision_id = $1
         order by queued_at desc, id desc
         limit 1",
    )
    .bind(knowledge_revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_ingest_jobs_by_knowledge_document_id(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    knowledge_document_id: Uuid,
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and knowledge_document_id = $3
         order by queued_at desc, id desc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(knowledge_document_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_ingest_jobs_by_mutation_ids(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    mutation_ids: &[Uuid],
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    if mutation_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, IngestJobRow>(
        "select
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at
         from ingest_job
         where workspace_id = $1
           and library_id = $2
           and mutation_id = any($3)
         order by queued_at desc, id desc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(mutation_ids)
    .fetch_all(postgres)
    .await
}

pub async fn list_ingest_jobs(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<IngestJobRow>, sqlx::Error> {
    let effective_limit = limit.unwrap_or(500);
    let effective_offset = offset.unwrap_or(0);

    match (workspace_id, library_id) {
        (Some(workspace_id), Some(library_id)) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
                    connector_id,
                    async_operation_id,
                    knowledge_document_id,
                    knowledge_revision_id,
                    job_kind::text as job_kind,
                    queue_state::text as queue_state,
                    priority,
                    dedupe_key,
                    queued_at,
                    available_at,
                    completed_at
                 from ingest_job
                 where workspace_id = $1
                   and library_id = $2
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $3 offset $4",
            )
            .bind(workspace_id)
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (Some(workspace_id), None) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
                    connector_id,
                    async_operation_id,
                    knowledge_document_id,
                    knowledge_revision_id,
                    job_kind::text as job_kind,
                    queue_state::text as queue_state,
                    priority,
                    dedupe_key,
                    queued_at,
                    available_at,
                    completed_at
                 from ingest_job
                 where workspace_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )
            .bind(workspace_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
                    connector_id,
                    async_operation_id,
                    knowledge_document_id,
                    knowledge_revision_id,
                    job_kind::text as job_kind,
                    queue_state::text as queue_state,
                    priority,
                    dedupe_key,
                    queued_at,
                    available_at,
                    completed_at
                 from ingest_job
                 where library_id = $1
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $2 offset $3",
            )
            .bind(library_id)
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, IngestJobRow>(
                "select
                    id,
                    workspace_id,
                    library_id,
                    mutation_id,
                    connector_id,
                    async_operation_id,
                    knowledge_document_id,
                    knowledge_revision_id,
                    job_kind::text as job_kind,
                    queue_state::text as queue_state,
                    priority,
                    dedupe_key,
                    queued_at,
                    available_at,
                    completed_at
                 from ingest_job
                 order by priority asc, available_at asc, queued_at asc, id asc
                 limit $1 offset $2",
            )
            .bind(effective_limit)
            .bind(effective_offset)
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn update_ingest_job(
    postgres: &PgPool,
    job_id: Uuid,
    input: &UpdateIngestJob,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "update ingest_job
         set mutation_id = $2,
             connector_id = $3,
             async_operation_id = $4,
             knowledge_document_id = $5,
             knowledge_revision_id = $6,
             job_kind = $7::ingest_job_kind,
             queue_state = $8::ingest_queue_state,
             priority = $9,
             dedupe_key = $10,
             available_at = $11,
             completed_at = $12
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at",
    )
    .bind(job_id)
    .bind(input.mutation_id)
    .bind(input.connector_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(&input.job_kind)
    .bind(&input.queue_state)
    .bind(input.priority)
    .bind(&input.dedupe_key)
    .bind(input.available_at)
    .bind(input.completed_at)
    .fetch_optional(postgres)
    .await
}

pub async fn create_ingest_attempt(
    postgres: &PgPool,
    input: &NewIngestAttempt,
) -> Result<IngestAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "insert into ingest_attempt (
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7::ingest_attempt_state,
            $8,
            coalesce($9, now()),
            $10,
            $11,
            $12,
            $13,
            $14
        )
        returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable",
    )
    .bind(Uuid::now_v7())
    .bind(input.job_id)
    .bind(input.attempt_number)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.started_at)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(input.retryable)
    .fetch_one(postgres)
    .await
}

pub async fn get_ingest_attempt_by_id(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable
         from ingest_attempt
         where id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_ingest_attempts_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Vec<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable
         from ingest_attempt
         where job_id = $1
         order by attempt_number asc, started_at asc, id asc",
    )
    .bind(job_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_latest_ingest_attempt_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "select
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable
         from ingest_attempt
         where job_id = $1
         order by attempt_number desc, started_at desc, id desc
         limit 1",
    )
    .bind(job_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_latest_ingest_attempts_by_job_ids(
    postgres: &PgPool,
    job_ids: &[Uuid],
) -> Result<Vec<IngestAttemptRow>, sqlx::Error> {
    if job_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, IngestAttemptRow>(
        "select distinct on (job_id)
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable
         from ingest_attempt
         where job_id = any($1)
         order by job_id, attempt_number desc, started_at desc, id desc",
    )
    .bind(job_ids)
    .fetch_all(postgres)
    .await
}

pub async fn update_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
    input: &UpdateIngestAttempt,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestAttemptRow>(
        "update ingest_attempt
         set worker_principal_id = $2,
             lease_token = $3,
             knowledge_generation_id = $4,
             attempt_state = $5::ingest_attempt_state,
             current_stage = $6,
             heartbeat_at = $7,
             finished_at = $8,
             failure_class = $9,
             failure_code = $10,
             retryable = $11
         where id = $1
         returning
            id,
            job_id,
            attempt_number,
            worker_principal_id,
            lease_token,
            knowledge_generation_id,
            attempt_state::text as attempt_state,
            current_stage,
            started_at,
            heartbeat_at,
            finished_at,
            failure_class,
            failure_code,
            retryable",
    )
    .bind(attempt_id)
    .bind(input.worker_principal_id)
    .bind(&input.lease_token)
    .bind(input.knowledge_generation_id)
    .bind(&input.attempt_state)
    .bind(&input.current_stage)
    .bind(input.heartbeat_at)
    .bind(input.finished_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(input.retryable)
    .fetch_optional(postgres)
    .await
}

/// Lightweight heartbeat touch — only updates `heartbeat_at` and optionally
/// `current_stage` on an `ingest_attempt` row.
pub async fn touch_attempt_heartbeat(
    postgres: &PgPool,
    attempt_id: Uuid,
    current_stage: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingest_attempt
         set heartbeat_at = now(),
             current_stage = coalesce($2, current_stage)
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .bind(current_stage)
    .execute(postgres)
    .await?;
    Ok(())
}

pub async fn create_ingest_stage_event(
    postgres: &PgPool,
    input: &NewIngestStageEvent,
) -> Result<IngestStageEventRow, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "insert into ingest_stage_event (
            id,
            attempt_id,
            stage_name,
            stage_state,
            ordinal,
            message,
            details_json,
            recorded_at
        )
        values (
            $1,
            $2,
            $3,
            $4::ingest_stage_state,
            $5,
            $6,
            $7,
            coalesce($8, now())
        )
        returning
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.attempt_id)
    .bind(&input.stage_name)
    .bind(&input.stage_state)
    .bind(input.ordinal)
    .bind(&input.message)
    .bind(&input.details_json)
    .bind(input.recorded_at)
    .fetch_one(postgres)
    .await
}

pub async fn get_ingest_stage_event_by_id(
    postgres: &PgPool,
    event_id: Uuid,
) -> Result<Option<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at
         from ingest_stage_event
         where id = $1",
    )
    .bind(event_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_ingest_stage_events_by_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Vec<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            id,
            attempt_id,
            stage_name,
            stage_state::text as stage_state,
            ordinal,
            message,
            details_json,
            recorded_at
         from ingest_stage_event
         where attempt_id = $1
         order by ordinal asc, recorded_at asc, id asc",
    )
    .bind(attempt_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_ingest_stage_events_by_job(
    postgres: &PgPool,
    job_id: Uuid,
) -> Result<Vec<IngestStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestStageEventRow>(
        "select
            event.id,
            event.attempt_id,
            event.stage_name,
            event.stage_state::text as stage_state,
            event.ordinal,
            event.message,
            event.details_json,
            event.recorded_at
         from ingest_stage_event as event
         join ingest_attempt as attempt on attempt.id = event.attempt_id
         where attempt.job_id = $1
         order by attempt.attempt_number asc, event.ordinal asc, event.recorded_at asc, event.id asc",
    )
    .bind(job_id)
    .fetch_all(postgres)
    .await
}

/// Atomically claim the next available canonical `ingest_job` by transitioning
/// its `queue_state` from `queued` → `leased`.  Uses `FOR UPDATE SKIP LOCKED`
/// so concurrent workers never claim the same row.
pub async fn claim_next_queued_ingest_job(
    postgres: &PgPool,
) -> Result<Option<IngestJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestJobRow>(
        "update ingest_job
         set queue_state = 'leased'::ingest_queue_state
         where id = (
             select id from ingest_job
             where queue_state = 'queued'
               and available_at <= now()
             order by priority asc, available_at asc, queued_at asc, id asc
             limit 1
             for update skip locked
         )
         returning
            id,
            workspace_id,
            library_id,
            mutation_id,
            connector_id,
            async_operation_id,
            knowledge_document_id,
            knowledge_revision_id,
            job_kind::text as job_kind,
            queue_state::text as queue_state,
            priority,
            dedupe_key,
            queued_at,
            available_at,
            completed_at",
    )
    .fetch_optional(postgres)
    .await
}

/// Recover canonical `ingest_job` rows stuck in `leased` whose corresponding
/// `ingest_attempt` heartbeat is stale (older than `stale_threshold`).
/// Returns the number of jobs reset back to `queued`.
pub async fn recover_stale_canonical_leases(
    postgres: &PgPool,
    stale_threshold: chrono::Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now() - stale_threshold;
    let result = sqlx::query(
        "with stale_attempts as (
             select distinct a.job_id
             from ingest_attempt a
             join ingest_job j on j.id = a.job_id
             where j.queue_state = 'leased'
               and a.attempt_state = 'leased'
               and a.heartbeat_at < $1
         ),
         failed_attempts as (
             update ingest_attempt
             set attempt_state = 'failed',
                 failure_class = 'lease_expired',
                 failure_code = 'stale_heartbeat',
                 finished_at = now(),
                 retryable = true
             where job_id in (select job_id from stale_attempts)
               and attempt_state = 'leased'
         )
         update ingest_job
         set queue_state = 'queued',
             available_at = now()
         where id in (select job_id from stale_attempts)",
    )
    .bind(cutoff)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

pub async fn cancel_queued_jobs_for_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ingest_job
         SET queue_state = 'cancelled', completed_at = now()
         WHERE mutation_id IN (
             SELECT m.id FROM content_mutation m
             JOIN content_mutation_item mi ON mi.mutation_id = m.id
             WHERE mi.document_id = $1
         )
         AND queue_state IN ('queued', 'available')
         AND completed_at IS NULL",
    )
    .bind(document_id)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}
