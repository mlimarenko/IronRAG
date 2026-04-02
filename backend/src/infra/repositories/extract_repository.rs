use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct ExtractContentRow {
    pub revision_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub extract_state: String,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub warning_count: i32,
    pub preparation_state: Option<String>,
    pub preparation_checkpoint_json: serde_json::Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ExtractChunkResultRow {
    pub id: Uuid,
    pub chunk_id: Uuid,
    pub attempt_id: Uuid,
    pub extract_state: String,
    pub provider_call_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ExtractNodeCandidateRow {
    pub id: Uuid,
    pub chunk_result_id: Uuid,
    pub canonical_key: String,
    pub node_kind: String,
    pub display_label: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ExtractEdgeCandidateRow {
    pub id: Uuid,
    pub chunk_result_id: Uuid,
    pub canonical_key: String,
    pub edge_kind: String,
    pub from_canonical_key: String,
    pub to_canonical_key: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ExtractResumeCursorRow {
    pub attempt_id: Uuid,
    pub last_completed_chunk_index: i32,
    pub replay_count: i32,
    pub downgrade_level: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewExtractNodeCandidate<'a> {
    pub canonical_key: &'a str,
    pub node_kind: &'a str,
    pub display_label: &'a str,
    pub summary: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewExtractEdgeCandidate<'a> {
    pub canonical_key: &'a str,
    pub edge_kind: &'a str,
    pub from_canonical_key: &'a str,
    pub to_canonical_key: &'a str,
    pub summary: Option<&'a str>,
}

pub async fn get_extract_content_by_revision_id(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Option<ExtractContentRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractContentRow>(
        "select
            revision_id,
            attempt_id,
            extract_state::text as extract_state,
            normalized_text,
            text_checksum,
            warning_count,
            preparation_state,
            preparation_checkpoint_json,
            updated_at
         from extract_content
         where revision_id = $1",
    )
    .bind(revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_extract_content_by_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Vec<ExtractContentRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractContentRow>(
        "select
            revision_id,
            attempt_id,
            extract_state::text as extract_state,
            normalized_text,
            text_checksum,
            warning_count,
            preparation_state,
            preparation_checkpoint_json,
            updated_at
         from extract_content
         where attempt_id = $1
         order by updated_at desc, revision_id asc",
    )
    .bind(attempt_id)
    .fetch_all(postgres)
    .await
}

pub async fn upsert_extract_content(
    postgres: &PgPool,
    revision_id: Uuid,
    attempt_id: Option<Uuid>,
    extract_state: &str,
    normalized_text: Option<&str>,
    text_checksum: Option<&str>,
    warning_count: i32,
    preparation_state: Option<&str>,
    preparation_checkpoint_json: serde_json::Value,
) -> Result<ExtractContentRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractContentRow>(
        "insert into extract_content (
            revision_id,
            attempt_id,
            extract_state,
            normalized_text,
            text_checksum,
            warning_count,
            preparation_state,
            preparation_checkpoint_json,
            updated_at
        )
        values ($1, $2, $3::extract_state, $4, $5, $6, $7, $8, now())
        on conflict (revision_id)
        do update set attempt_id = excluded.attempt_id,
                      extract_state = excluded.extract_state,
                      normalized_text = excluded.normalized_text,
                      text_checksum = excluded.text_checksum,
                      warning_count = excluded.warning_count,
                      preparation_state = excluded.preparation_state,
                      preparation_checkpoint_json = excluded.preparation_checkpoint_json,
                      updated_at = now()
        returning
            revision_id,
            attempt_id,
            extract_state::text as extract_state,
            normalized_text,
            text_checksum,
            warning_count,
            preparation_state,
            preparation_checkpoint_json,
            updated_at",
    )
    .bind(revision_id)
    .bind(attempt_id)
    .bind(extract_state)
    .bind(normalized_text)
    .bind(text_checksum)
    .bind(warning_count)
    .bind(preparation_state)
    .bind(preparation_checkpoint_json)
    .fetch_one(postgres)
    .await
}

pub async fn get_extract_chunk_result_by_chunk_and_attempt(
    postgres: &PgPool,
    chunk_id: Uuid,
    attempt_id: Uuid,
) -> Result<Option<ExtractChunkResultRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "select
            id,
            chunk_id,
            attempt_id,
            extract_state::text as extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code
         from extract_chunk_result
         where chunk_id = $1
           and attempt_id = $2",
    )
    .bind(chunk_id)
    .bind(attempt_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_extract_chunk_result_by_id(
    postgres: &PgPool,
    chunk_result_id: Uuid,
) -> Result<Option<ExtractChunkResultRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "select
            id,
            chunk_id,
            attempt_id,
            extract_state::text as extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code
         from extract_chunk_result
         where id = $1",
    )
    .bind(chunk_result_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_extract_chunk_results_by_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Vec<ExtractChunkResultRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "select
            id,
            chunk_id,
            attempt_id,
            extract_state::text as extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code
         from extract_chunk_result
         where attempt_id = $1
         order by started_at asc, id asc",
    )
    .bind(attempt_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_ready_extract_chunk_results_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Vec<ExtractChunkResultRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "select distinct on (result.chunk_id)
            result.id,
            result.chunk_id,
            result.attempt_id,
            result.extract_state::text as extract_state,
            result.provider_call_id,
            result.started_at,
            result.finished_at,
            result.failure_code
         from extract_chunk_result result
         join content_chunk chunk on chunk.id = result.chunk_id
         where chunk.revision_id = $1
           and result.extract_state = 'ready'::extract_state
         order by
            result.chunk_id asc,
            coalesce(result.finished_at, result.started_at) desc,
            result.started_at desc,
            result.id desc",
    )
    .bind(revision_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_extract_chunk_result(
    postgres: &PgPool,
    chunk_id: Uuid,
    attempt_id: Uuid,
    extract_state: &str,
    provider_call_id: Option<Uuid>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    failure_code: Option<&str>,
) -> Result<ExtractChunkResultRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "insert into extract_chunk_result (
            id,
            chunk_id,
            attempt_id,
            extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code
        )
        values ($1, $2, $3, $4::extract_state, $5, coalesce($6, now()), $7, $8)
        returning
            id,
            chunk_id,
            attempt_id,
            extract_state::text as extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code",
    )
    .bind(Uuid::now_v7())
    .bind(chunk_id)
    .bind(attempt_id)
    .bind(extract_state)
    .bind(provider_call_id)
    .bind(started_at)
    .bind(finished_at)
    .bind(failure_code)
    .fetch_one(postgres)
    .await
}

pub async fn update_extract_chunk_result(
    postgres: &PgPool,
    chunk_result_id: Uuid,
    extract_state: &str,
    provider_call_id: Option<Uuid>,
    finished_at: Option<DateTime<Utc>>,
    failure_code: Option<&str>,
) -> Result<Option<ExtractChunkResultRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractChunkResultRow>(
        "update extract_chunk_result
         set extract_state = $2::extract_state,
             provider_call_id = $3,
             finished_at = $4,
             failure_code = $5
         where id = $1
         returning
            id,
            chunk_id,
            attempt_id,
            extract_state::text as extract_state,
            provider_call_id,
            started_at,
            finished_at,
            failure_code",
    )
    .bind(chunk_result_id)
    .bind(extract_state)
    .bind(provider_call_id)
    .bind(finished_at)
    .bind(failure_code)
    .fetch_optional(postgres)
    .await
}

pub async fn list_extract_node_candidates_by_chunk_result(
    postgres: &PgPool,
    chunk_result_id: Uuid,
) -> Result<Vec<ExtractNodeCandidateRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractNodeCandidateRow>(
        "select
            id,
            chunk_result_id,
            canonical_key,
            node_kind,
            display_label,
            summary
         from extract_node_candidate
         where chunk_result_id = $1
         order by canonical_key asc, id asc",
    )
    .bind(chunk_result_id)
    .fetch_all(postgres)
    .await
}

pub async fn replace_extract_node_candidates(
    postgres: &PgPool,
    chunk_result_id: Uuid,
    candidates: &[NewExtractNodeCandidate<'_>],
) -> Result<Vec<ExtractNodeCandidateRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;

    sqlx::query("delete from extract_node_candidate where chunk_result_id = $1")
        .bind(chunk_result_id)
        .execute(&mut *transaction)
        .await?;

    let mut rows = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let row = sqlx::query_as::<_, ExtractNodeCandidateRow>(
            "insert into extract_node_candidate (
                id,
                chunk_result_id,
                canonical_key,
                node_kind,
                display_label,
                summary
            )
            values ($1, $2, $3, $4, $5, $6)
            returning
                id,
                chunk_result_id,
                canonical_key,
                node_kind,
                display_label,
                summary",
        )
        .bind(Uuid::now_v7())
        .bind(chunk_result_id)
        .bind(candidate.canonical_key)
        .bind(candidate.node_kind)
        .bind(candidate.display_label)
        .bind(candidate.summary)
        .fetch_one(&mut *transaction)
        .await?;
        rows.push(row);
    }

    transaction.commit().await?;
    Ok(rows)
}

pub async fn list_extract_edge_candidates_by_chunk_result(
    postgres: &PgPool,
    chunk_result_id: Uuid,
) -> Result<Vec<ExtractEdgeCandidateRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractEdgeCandidateRow>(
        "select
            id,
            chunk_result_id,
            canonical_key,
            edge_kind,
            from_canonical_key,
            to_canonical_key,
            summary
         from extract_edge_candidate
         where chunk_result_id = $1
         order by canonical_key asc, id asc",
    )
    .bind(chunk_result_id)
    .fetch_all(postgres)
    .await
}

pub async fn replace_extract_edge_candidates(
    postgres: &PgPool,
    chunk_result_id: Uuid,
    candidates: &[NewExtractEdgeCandidate<'_>],
) -> Result<Vec<ExtractEdgeCandidateRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;

    sqlx::query("delete from extract_edge_candidate where chunk_result_id = $1")
        .bind(chunk_result_id)
        .execute(&mut *transaction)
        .await?;

    let mut rows = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let row = sqlx::query_as::<_, ExtractEdgeCandidateRow>(
            "insert into extract_edge_candidate (
                id,
                chunk_result_id,
                canonical_key,
                edge_kind,
                from_canonical_key,
                to_canonical_key,
                summary
            )
            values ($1, $2, $3, $4, $5, $6, $7)
            returning
                id,
                chunk_result_id,
                canonical_key,
                edge_kind,
                from_canonical_key,
                to_canonical_key,
                summary",
        )
        .bind(Uuid::now_v7())
        .bind(chunk_result_id)
        .bind(candidate.canonical_key)
        .bind(candidate.edge_kind)
        .bind(candidate.from_canonical_key)
        .bind(candidate.to_canonical_key)
        .bind(candidate.summary)
        .fetch_one(&mut *transaction)
        .await?;
        rows.push(row);
    }

    transaction.commit().await?;
    Ok(rows)
}

pub async fn get_extract_resume_cursor_by_attempt_id(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<Option<ExtractResumeCursorRow>, sqlx::Error> {
    sqlx::query_as::<_, ExtractResumeCursorRow>(
        "select
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at
         from extract_resume_cursor
         where attempt_id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(postgres)
    .await
}

pub async fn upsert_extract_resume_cursor(
    postgres: &PgPool,
    attempt_id: Uuid,
    last_completed_chunk_index: i32,
    replay_count: i32,
    downgrade_level: i32,
) -> Result<ExtractResumeCursorRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractResumeCursorRow>(
        "insert into extract_resume_cursor (
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at
        )
        values ($1, $2, $3, $4, now())
        on conflict (attempt_id)
        do update set last_completed_chunk_index = excluded.last_completed_chunk_index,
                      replay_count = excluded.replay_count,
                      downgrade_level = excluded.downgrade_level,
                      updated_at = now()
        returning
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at",
    )
    .bind(attempt_id)
    .bind(last_completed_chunk_index)
    .bind(replay_count)
    .bind(downgrade_level)
    .fetch_one(postgres)
    .await
}

pub async fn checkpoint_extract_resume_cursor(
    postgres: &PgPool,
    attempt_id: Uuid,
    last_completed_chunk_index: i32,
) -> Result<ExtractResumeCursorRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractResumeCursorRow>(
        "insert into extract_resume_cursor (
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at
        )
        values ($1, $2, 0, 0, now())
        on conflict (attempt_id)
        do update set last_completed_chunk_index =
                          greatest(
                              extract_resume_cursor.last_completed_chunk_index,
                              excluded.last_completed_chunk_index
                          ),
                      updated_at = now()
        returning
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at",
    )
    .bind(attempt_id)
    .bind(last_completed_chunk_index)
    .fetch_one(postgres)
    .await
}

pub async fn increment_extract_resume_replay_count(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<ExtractResumeCursorRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractResumeCursorRow>(
        "insert into extract_resume_cursor (
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at
        )
        values ($1, -1, 1, 0, now())
        on conflict (attempt_id)
        do update set replay_count = extract_resume_cursor.replay_count + 1,
                      updated_at = now()
        returning
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at",
    )
    .bind(attempt_id)
    .fetch_one(postgres)
    .await
}

pub async fn increment_extract_resume_downgrade_level(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<ExtractResumeCursorRow, sqlx::Error> {
    sqlx::query_as::<_, ExtractResumeCursorRow>(
        "insert into extract_resume_cursor (
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at
        )
        values ($1, -1, 0, 1, now())
        on conflict (attempt_id)
        do update set downgrade_level = extract_resume_cursor.downgrade_level + 1,
                      updated_at = now()
        returning
            attempt_id,
            last_completed_chunk_index,
            replay_count,
            downgrade_level,
            updated_at",
    )
    .bind(attempt_id)
    .fetch_one(postgres)
    .await
}
