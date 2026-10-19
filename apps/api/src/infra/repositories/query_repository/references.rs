use super::*;

/// One row that will land in `query_chunk_reference`. The turn layer
/// captures these in `RuntimeStructuredQueryResult.chunk_references`
/// and forwards them to `append_chunk_references` once it has an
/// `execution_id`. Keeping the repo-layer type small and insert-only
/// avoids leaking the internal `RuntimeMatchedChunk` into the query
/// repository surface.
#[derive(Debug, Clone, Copy)]
pub struct NewQueryChunkReference {
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

/// Persist the final ranked chunks that shaped a query execution's
/// answer context into `query_chunk_reference`. The write is an UNNEST
/// batch insert — one Postgres round-trip regardless of how many
/// chunks landed in the bundle. `ON CONFLICT DO NOTHING` makes the
/// call idempotent if the turn layer ever retries after a transient
/// failure on a later step (the caller holds the `execution_id` so a
/// replay cannot produce mismatched rows).
///
/// No-op when `references` is empty — avoids a redundant round-trip
/// on turns that produced no retrieved chunks.
pub async fn append_chunk_references(
    postgres: &PgPool,
    execution_id: Uuid,
    references: &[NewQueryChunkReference],
) -> Result<u64, sqlx::Error> {
    if references.is_empty() {
        return Ok(0);
    }
    let chunk_ids: Vec<Uuid> = references.iter().map(|reference| reference.chunk_id).collect();
    let ranks: Vec<i32> = references.iter().map(|reference| reference.rank).collect();
    let scores: Vec<f64> = references.iter().map(|reference| reference.score).collect();
    let result = sqlx::query(
        "insert into query_chunk_reference (execution_id, chunk_id, rank, score)
         select $1, chunk_id, rank, score
         from unnest($2::uuid[], $3::int[], $4::double precision[])
              as input(chunk_id, rank, score)
         on conflict (execution_id, chunk_id) do nothing",
    )
    .bind(execution_id)
    .bind(&chunk_ids)
    .bind(&ranks)
    .bind(&scores)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes persisted query references that point at knowledge contributed by one document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the persisted query references.
pub async fn delete_query_execution_references_by_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               join content_revision
                 on content_revision.id = content_chunk.revision_id
               where content_chunk.id = reference.chunk_id
                 and content_revision.document_id = $2
           )",
    )
    .bind(library_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(chunk_result.rows_affected())
}

/// Deletes persisted query references that point at knowledge contributed by one content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting revision-scoped query references.
pub async fn delete_query_execution_references_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    delete_query_execution_references_by_content_revision_with_executor(
        pool,
        library_id,
        document_id,
        revision_id,
    )
    .await
}

pub async fn delete_query_execution_references_by_content_revision_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               join content_revision
                 on content_revision.id = content_chunk.revision_id
               where content_chunk.id = reference.chunk_id
                 and content_revision.document_id = $2
                 and content_chunk.revision_id = $3
           )",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(executor)
    .await?;
    Ok(chunk_result.rows_affected())
}
