use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::infra::knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority};

use super::content_document_repository::lock_document_by_id_with_executor;

const LIST_PREPARED_CHUNKS_BY_REVISION_SQL: &str = "select
        chunk.id,
        chunk.revision_id,
        chunk.chunk_index,
        chunk.start_offset,
        chunk.end_offset,
        chunk.token_count,
        chunk.normalized_text,
        chunk.text_checksum,
        chunk.occurred_at,
        chunk.occurred_until
     from content_chunk as chunk
     where chunk.revision_id = $1
       and exists (
           select 1
           from knowledge_structured_revision as structured
           where structured.revision_id = chunk.revision_id
             and structured.preparation_state = 'prepared'
       )
     order by chunk.chunk_index asc";

#[derive(Debug, Clone, FromRow)]
pub struct ContentChunkRow {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: String,
    pub text_checksum: String,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; NULL for non-temporal sources like PDF/image/markdown).
    pub occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. For
    /// single-record chunks `occurred_until == occurred_at`. NULL when
    /// `occurred_at` is NULL.
    pub occurred_until: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone)]
pub struct NewContentChunk<'a> {
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: &'a str,
    pub text_checksum: &'a str,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; None for non-temporal sources). Computed through the shared
    /// `record_jsonl::extract_chunk_temporal_bounds` helper.
    pub occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks; None when `occurred_at`
    /// is None.
    pub occurred_until: Option<DateTime<Utc>>,
}

/// Complete source + query-projection chunk payload. Owned values keep the
/// atomic replacement API free of parallel slices whose ordering could drift.
#[derive(Debug, Clone)]
pub struct NewContentChunkProjection {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub chunk_kind: Option<String>,
    pub content_text: String,
    pub normalized_text: String,
    pub text_checksum: String,
    pub support_block_ids: Vec<Uuid>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub literal_digest: Option<String>,
    pub chunk_state: String,
    pub text_generation: Option<i64>,
    pub vector_generation: Option<i64>,
    pub quality_score: Option<f32>,
    pub window_text: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub occurred_until: Option<DateTime<Utc>>,
}
pub async fn list_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    list_chunks_by_revision_with_executor(postgres, revision_id).await
}

pub async fn list_chunks_by_revision_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
) -> Result<Vec<ContentChunkRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentChunkRow>(
        "select
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
         from content_chunk
         where revision_id = $1
         order by chunk_index asc",
    )
    .bind(revision_id)
    .fetch_all(executor)
    .await
}

/// Lists source chunks only while the exact structured revision is finalized.
///
/// The readiness predicate and chunk rows deliberately share one Postgres
/// statement snapshot. A separate readiness read followed by this chunk read
/// could admit a replacement that committed between the two statements.
pub async fn list_prepared_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    list_prepared_chunks_by_revision_with_executor(postgres, revision_id).await
}

pub async fn list_prepared_chunks_by_revision_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
) -> Result<Vec<ContentChunkRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentChunkRow>(LIST_PREPARED_CHUNKS_BY_REVISION_SQL)
        .bind(revision_id)
        .fetch_all(executor)
        .await
}

pub async fn count_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("select count(*) from content_chunk where revision_id = $1")
        .bind(revision_id)
        .fetch_one(postgres)
        .await
}

/// Loads one source chunk by id.
///
/// # Errors
/// Returns any `SQLx` error raised while reading the row.
pub async fn get_chunk_by_id(
    postgres: &PgPool,
    chunk_id: Uuid,
) -> Result<Option<ContentChunkRow>, sqlx::Error> {
    get_chunk_by_id_with_executor(postgres, chunk_id).await
}

/// Loads one source chunk by id with the supplied executor so callers can keep
/// the read inside a wider transaction snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while reading the row.
pub async fn get_chunk_by_id_with_executor<'e, E>(
    executor: E,
    chunk_id: Uuid,
) -> Result<Option<ContentChunkRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentChunkRow>(
        "select
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
         from content_chunk
         where id = $1",
    )
    .bind(chunk_id)
    .fetch_optional(executor)
    .await
}

pub async fn create_chunk(
    postgres: &PgPool,
    new_chunk: &NewContentChunk<'_>,
) -> Result<ContentChunkRow, sqlx::Error> {
    sqlx::query_as::<_, ContentChunkRow>(
        "insert into content_chunk (
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        returning
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until",
    )
    .bind(Uuid::now_v7())
    .bind(new_chunk.revision_id)
    .bind(new_chunk.chunk_index)
    .bind(new_chunk.start_offset)
    .bind(new_chunk.end_offset)
    .bind(new_chunk.token_count)
    .bind(new_chunk.normalized_text)
    .bind(new_chunk.text_checksum)
    .bind(new_chunk.occurred_at)
    .bind(new_chunk.occurred_until)
    .fetch_one(postgres)
    .await
}

const CONTENT_CHUNK_ID_NAMESPACE: Uuid = Uuid::from_u128(0x6f44_2a36_0f5d_4f18_8f6c_f11d_f356_8f5a);

pub async fn delete_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query("delete from content_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(postgres)
        .await
        .map(|result| result.rows_affected())
}

/// Unconditional operator/test replacement of source and query-facing chunks
/// in one transaction. Production ingest uses
/// [`replace_chunks_with_projection_fenced`].
///
/// Vector rows are dimension-sharded runtime artifacts and are intentionally
/// removed by the caller before this transaction; that operation is fallible
/// and propagated. The durable text rows below either both commit or both stay
/// unchanged, including when a projection FK/constraint rejects the batch.
pub async fn replace_chunks_with_projection(
    postgres: &PgPool,
    revision_id: Uuid,
    chunks: &[NewContentChunkProjection],
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    let created_chunks = replace_chunks_with_projection_in_transaction(
        &mut transaction,
        revision_id,
        document_id,
        workspace_id,
        library_id,
        chunks,
    )
    .await?;
    transaction.commit().await?;
    Ok(created_chunks)
}

/// Production ingest replacement. Authority is checked while the parent,
/// attempt, revision, and document locks remain held through commit.
pub async fn replace_chunks_with_projection_fenced(
    postgres: &PgPool,
    authority: IngestRevisionWriteAuthority,
    chunks: &[NewContentChunkProjection],
) -> Result<IngestAttemptMutationOutcome<Vec<ContentChunkRow>>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        authority.workspace_id,
        authority.library_id,
    )
    .await?
    {
        transaction.commit().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    if !super::ingest_repository::lock_latest_leased_revision_attempt(
        &mut transaction,
        authority.library_id,
        authority.attempt_id,
        authority.revision_id,
    )
    .await?
    {
        transaction.commit().await?;
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    let created_chunks = replace_chunks_with_projection_in_transaction(
        &mut transaction,
        authority.revision_id,
        chunks.first().map_or(Uuid::nil(), |chunk| chunk.document_id),
        authority.workspace_id,
        authority.library_id,
        chunks,
    )
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(created_chunks))
}

async fn replace_chunks_with_projection_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    revision_id: Uuid,
    expected_document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    chunks: &[NewContentChunkProjection],
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    let Some((document_id, observed_workspace_id, observed_library_id)) =
        sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
            "select document_id, workspace_id, library_id
             from content_revision
             where id = $1
             for update",
        )
        .bind(revision_id)
        .fetch_optional(&mut **transaction)
        .await?
    else {
        return Err(sqlx::Error::RowNotFound);
    };
    if observed_workspace_id != workspace_id
        || observed_library_id != library_id
        || (expected_document_id != Uuid::nil() && expected_document_id != document_id)
    {
        return Err(sqlx::Error::Protocol(
            "chunk replacement authority does not match its current revision scope".to_string(),
        ));
    }
    let locked_document =
        lock_document_by_id_with_executor(&mut **transaction, document_id).await?;
    if locked_document.as_ref().is_none_or(|document| {
        document.workspace_id != workspace_id || document.library_id != library_id
    }) {
        return Err(sqlx::Error::RowNotFound);
    }
    if chunks.iter().any(|chunk| {
        chunk.revision_id != revision_id
            || chunk.document_id != document_id
            || chunk.workspace_id != workspace_id
            || chunk.library_id != library_id
            || chunk.start_offset < 0
            || chunk.end_offset < chunk.start_offset
    }) {
        return Err(sqlx::Error::Protocol(
            "chunk projection payload does not match its current revision scope".to_string(),
        ));
    }

    sqlx::query("delete from knowledge_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(&mut **transaction)
        .await?;
    sqlx::query("delete from content_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(&mut **transaction)
        .await?;

    const POSTGRES_MAX_BIND_PARAMETERS: usize = 65_535;
    const CONTENT_CHUNK_BIND_COUNT: usize = 10;
    const KNOWLEDGE_CHUNK_BIND_COUNT: usize = 23;
    let mut created_chunks = Vec::with_capacity(chunks.len());
    for batch in chunks.chunks(POSTGRES_MAX_BIND_PARAMETERS / CONTENT_CHUNK_BIND_COUNT) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum, occurred_at, occurred_until
             ) ",
        );
        builder.push_values(batch, |mut row, chunk| {
            row.push_bind(deterministic_content_chunk_projection_id(chunk))
                .push_bind(chunk.revision_id)
                .push_bind(chunk.chunk_index)
                .push_bind(chunk.start_offset)
                .push_bind(chunk.end_offset)
                .push_bind(chunk.token_count)
                .push_bind(&chunk.normalized_text)
                .push_bind(&chunk.text_checksum)
                .push_bind(chunk.occurred_at)
                .push_bind(chunk.occurred_until);
        });
        builder.push(
            " returning
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum, occurred_at, occurred_until",
        );
        created_chunks.extend(
            builder.build_query_as::<ContentChunkRow>().fetch_all(&mut **transaction).await?,
        );
    }
    for batch in chunks.chunks(POSTGRES_MAX_BIND_PARAMETERS / KNOWLEDGE_CHUNK_BIND_COUNT) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into knowledge_chunk (
                chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index,
                chunk_kind, content_text, normalized_text, span_start, span_end, token_count,
                support_block_ids, section_path, heading_trail, literal_digest, chunk_state,
                text_generation, vector_generation, quality_score, window_text,
                occurred_at, occurred_until
             ) ",
        );
        builder.push_values(batch, |mut row, chunk| {
            row.push_bind(deterministic_content_chunk_projection_id(chunk))
                .push_bind(chunk.workspace_id)
                .push_bind(chunk.library_id)
                .push_bind(chunk.document_id)
                .push_bind(chunk.revision_id)
                .push_bind(chunk.chunk_index)
                .push_bind(&chunk.chunk_kind)
                .push_bind(&chunk.content_text)
                .push_bind(&chunk.normalized_text)
                .push_bind(Some(chunk.start_offset))
                .push_bind(Some(chunk.end_offset))
                .push_bind(chunk.token_count)
                .push_bind(&chunk.support_block_ids)
                .push_bind(&chunk.section_path)
                .push_bind(&chunk.heading_trail)
                .push_bind(&chunk.literal_digest)
                .push_bind(&chunk.chunk_state)
                .push_bind(chunk.text_generation)
                .push_bind(chunk.vector_generation)
                .push_bind(chunk.quality_score)
                .push_bind(&chunk.window_text)
                .push_bind(chunk.occurred_at)
                .push_bind(chunk.occurred_until);
        });
        builder.build().execute(&mut **transaction).await?;
    }

    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut **transaction,
        library_id,
    )
    .await?;
    created_chunks.sort_by_key(|chunk| chunk.chunk_index);
    Ok(created_chunks)
}

fn deterministic_content_chunk_projection_id(chunk: &NewContentChunkProjection) -> Uuid {
    let name = format!("{}:{}:{}", chunk.revision_id, chunk.chunk_index, chunk.text_checksum);
    Uuid::new_v5(&CONTENT_CHUNK_ID_NAMESPACE, name.as_bytes())
}

#[cfg(test)]
#[path = "content_chunk_repository_tests.rs"]
mod tests;
