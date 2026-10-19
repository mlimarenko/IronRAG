use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
    repositories::catalog_repository,
};

#[derive(Debug, Clone, FromRow)]
pub struct ContentRevisionIngestUnitRow {
    pub revision_id: Uuid,
    pub stage_name: String,
    pub unit_ordinal: i32,
    pub unit_kind: String,
    pub range_start: i32,
    pub range_end: i32,
    pub unit_state: String,
    pub content_text: Option<String>,
    pub structure_hints_json: Option<Value>,
    pub source_metadata_json: Option<Value>,
    pub source_map_json: Option<Value>,
    pub warnings_json: Value,
    pub usage_json: Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub content_checksum: Option<String>,
    pub details_json: Value,
    pub attempt_id: Option<Uuid>,
    pub elapsed_ms: Option<i64>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentRevisionIngestUnitPageProgress {
    pub completed_page_count: i64,
    pub total_page_count: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ContentRevisionIngestUnitCompletedArtifact {
    pub stage_name: String,
    pub unit_ordinal: i32,
    pub unit_kind: String,
    pub range_start: i32,
    pub range_end: i32,
    pub content_text: Option<String>,
    pub structure_hints_json: Option<Value>,
    pub source_metadata_json: Option<Value>,
    pub source_map_json: Option<Value>,
    pub warnings_json: Value,
    pub usage_json: Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub content_checksum: Option<String>,
    pub details_json: Value,
    pub elapsed_ms: Option<i64>,
    pub started_at: Option<DateTime<Utc>>,
}

async fn lock_ingest_unit_write_authority(
    transaction: &mut Transaction<'_, Postgres>,
    authority: IngestRevisionWriteAuthority,
) -> Result<bool, sqlx::Error> {
    if !catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        authority.workspace_id,
        authority.library_id,
    )
    .await?
    {
        return Ok(false);
    }
    if !super::lock_latest_leased_revision_attempt(
        transaction,
        authority.library_id,
        authority.attempt_id,
        authority.revision_id,
    )
    .await?
    {
        return Ok(false);
    }
    let revision_locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from content_revision
         where id = $1
           and workspace_id = $2
           and library_id = $3
         for share",
    )
    .bind(authority.revision_id)
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(revision_locked.is_some())
}

async fn upsert_content_revision_ingest_unit_completed_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
    attempt_id: Option<Uuid>,
    input: &ContentRevisionIngestUnitCompletedArtifact,
) -> Result<ContentRevisionIngestUnitRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentRevisionIngestUnitRow>(
        "insert into content_revision_ingest_unit (
            revision_id,
            stage_name,
            unit_ordinal,
            unit_kind,
            range_start,
            range_end,
            unit_state,
            content_text,
            structure_hints_json,
            source_metadata_json,
            source_map_json,
            warnings_json,
            usage_json,
            provider_kind,
            model_name,
            content_checksum,
            details_json,
            attempt_id,
            elapsed_ms,
            started_at,
            completed_at,
            updated_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            'completed',
            $7,
            $8,
            $9,
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16,
            $17,
            $18,
            coalesce($19, now()),
            now(),
            now()
        )
        on conflict (revision_id, stage_name, unit_ordinal)
        do update set
            unit_kind = excluded.unit_kind,
            range_start = excluded.range_start,
            range_end = excluded.range_end,
            unit_state = 'completed',
            content_text = excluded.content_text,
            structure_hints_json = excluded.structure_hints_json,
            source_metadata_json = excluded.source_metadata_json,
            source_map_json = excluded.source_map_json,
            warnings_json = excluded.warnings_json,
            usage_json = excluded.usage_json,
            provider_kind = excluded.provider_kind,
            model_name = excluded.model_name,
            content_checksum = excluded.content_checksum,
            details_json = excluded.details_json,
            attempt_id = excluded.attempt_id,
            elapsed_ms = excluded.elapsed_ms,
            failure_code = null,
            failure_message = null,
            started_at = coalesce(content_revision_ingest_unit.started_at, excluded.started_at),
            completed_at = now(),
            updated_at = now()
        returning
            revision_id,
            stage_name,
            unit_ordinal,
            unit_kind,
            range_start,
            range_end,
            unit_state,
            content_text,
            structure_hints_json,
            source_metadata_json,
            source_map_json,
            warnings_json,
            usage_json,
            provider_kind,
            model_name,
            content_checksum,
            details_json,
            attempt_id,
            elapsed_ms,
            failure_code,
            failure_message,
            started_at,
            completed_at,
            updated_at",
    )
    .bind(revision_id)
    .bind(&input.stage_name)
    .bind(input.unit_ordinal)
    .bind(&input.unit_kind)
    .bind(input.range_start)
    .bind(input.range_end)
    .bind(&input.content_text)
    .bind(&input.structure_hints_json)
    .bind(&input.source_metadata_json)
    .bind(&input.source_map_json)
    .bind(&input.warnings_json)
    .bind(&input.usage_json)
    .bind(&input.provider_kind)
    .bind(&input.model_name)
    .bind(&input.content_checksum)
    .bind(&input.details_json)
    .bind(attempt_id)
    .bind(input.elapsed_ms)
    .bind(input.started_at)
    .fetch_one(executor)
    .await
}

pub async fn upsert_content_revision_ingest_unit_completed_fenced(
    postgres: &PgPool,
    authority: IngestRevisionWriteAuthority,
    input: &ContentRevisionIngestUnitCompletedArtifact,
) -> Result<IngestAttemptMutationOutcome<ContentRevisionIngestUnitRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !lock_ingest_unit_write_authority(&mut transaction, authority).await? {
        transaction.rollback().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    let row = upsert_content_revision_ingest_unit_completed_with_executor(
        &mut *transaction,
        authority.revision_id,
        Some(authority.attempt_id),
        input,
    )
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(row))
}

async fn list_content_revision_ingest_units_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
    stage_name: &str,
    attempt_id: Option<Uuid>,
) -> Result<Vec<ContentRevisionIngestUnitRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentRevisionIngestUnitRow>(
        "select
            revision_id,
            stage_name,
            unit_ordinal,
            unit_kind,
            range_start,
            range_end,
            unit_state,
            content_text,
            structure_hints_json,
            source_metadata_json,
            source_map_json,
            warnings_json,
            usage_json,
            provider_kind,
            model_name,
            content_checksum,
            details_json,
            attempt_id,
            elapsed_ms,
            failure_code,
            failure_message,
            started_at,
            completed_at,
            updated_at
         from content_revision_ingest_unit
         where revision_id = $1
           and stage_name = $2
           and ($3::uuid is null or attempt_id = $3)
         order by unit_ordinal asc",
    )
    .bind(revision_id)
    .bind(stage_name)
    .bind(attempt_id)
    .fetch_all(executor)
    .await
}

/// Unconditional operator/test read across attempts. Production reads must use
/// the fenced variant, which returns only units owned by the current attempt.
pub async fn list_content_revision_ingest_units_unfenced(
    postgres: &PgPool,
    revision_id: Uuid,
    stage_name: &str,
) -> Result<Vec<ContentRevisionIngestUnitRow>, sqlx::Error> {
    list_content_revision_ingest_units_with_executor(postgres, revision_id, stage_name, None).await
}

pub async fn list_content_revision_ingest_units_fenced(
    postgres: &PgPool,
    authority: IngestRevisionWriteAuthority,
    stage_name: &str,
) -> Result<IngestAttemptMutationOutcome<Vec<ContentRevisionIngestUnitRow>>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !lock_ingest_unit_write_authority(&mut transaction, authority).await? {
        transaction.rollback().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    let rows = list_content_revision_ingest_units_with_executor(
        &mut *transaction,
        authority.revision_id,
        stage_name,
        Some(authority.attempt_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(rows))
}

async fn sum_completed_content_revision_ingest_unit_pages_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
    stage_name: &str,
    attempt_id: Option<Uuid>,
) -> Result<i64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar::<_, i64>(
        "select coalesce(sum(greatest(range_end - range_start + 1, 0)), 0)::bigint
         from content_revision_ingest_unit
         where revision_id = $1
           and stage_name = $2
           and unit_kind = 'pdf_page_range'
           and unit_state = 'completed'
           and content_text is not null
           and content_checksum =
               'sha256:' || encode(digest(content_text, 'sha256'), 'hex')
           and ($3::uuid is null or attempt_id = $3)",
    )
    .bind(revision_id)
    .bind(stage_name)
    .bind(attempt_id)
    .fetch_one(executor)
    .await
}

pub async fn sum_completed_content_revision_ingest_unit_pages_fenced(
    postgres: &PgPool,
    authority: IngestRevisionWriteAuthority,
    stage_name: &str,
) -> Result<IngestAttemptMutationOutcome<i64>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !lock_ingest_unit_write_authority(&mut transaction, authority).await? {
        transaction.rollback().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    let completed_pages = sum_completed_content_revision_ingest_unit_pages_with_executor(
        &mut *transaction,
        authority.revision_id,
        stage_name,
        Some(authority.attempt_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(completed_pages))
}

async fn get_content_revision_ingest_unit_page_progress_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
    stage_name: &str,
    attempt_id: Option<Uuid>,
) -> Result<ContentRevisionIngestUnitPageProgress, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentRevisionIngestUnitPageProgress>(
        "select
            coalesce(
                sum(greatest(range_end - range_start + 1, 0))
                    filter (
                        where unit_state = 'completed'
                          and content_text is not null
                          and content_checksum =
                              'sha256:' || encode(digest(content_text, 'sha256'), 'hex')
                    ),
                0
            )::bigint as completed_page_count,
            max(
                case
                    when details_json ->> 'pageCount' ~ '^[0-9]+$'
                    then (details_json ->> 'pageCount')::bigint
                    else null
                end
            ) as total_page_count
         from content_revision_ingest_unit
         where revision_id = $1
           and stage_name = $2
           and unit_kind = 'pdf_page_range'
           and ($3::uuid is null or attempt_id = $3)",
    )
    .bind(revision_id)
    .bind(stage_name)
    .bind(attempt_id)
    .fetch_one(executor)
    .await
}

pub async fn get_content_revision_ingest_unit_page_progress_fenced(
    postgres: &PgPool,
    authority: IngestRevisionWriteAuthority,
    stage_name: &str,
) -> Result<IngestAttemptMutationOutcome<ContentRevisionIngestUnitPageProgress>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !lock_ingest_unit_write_authority(&mut transaction, authority).await? {
        transaction.rollback().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    let progress = get_content_revision_ingest_unit_page_progress_with_executor(
        &mut *transaction,
        authority.revision_id,
        stage_name,
        Some(authority.attempt_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(progress))
}
