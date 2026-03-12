use pgvector::Vector;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::infra::repositories::ChunkRow;

#[derive(Debug, Clone, FromRow)]
pub struct ScoredChunkRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub project_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
    pub metadata_json: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub distance: f64,
}

impl ScoredChunkRow {
    #[must_use]
    pub fn into_chunk(self) -> ChunkRow {
        ChunkRow {
            id: self.id,
            document_id: self.document_id,
            project_id: self.project_id,
            ordinal: self.ordinal,
            content: self.content,
            token_count: self.token_count,
            metadata_json: self.metadata_json,
            created_at: self.created_at,
        }
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn cosine_similarity_score(&self) -> f32 {
        (1.0_f64 - self.distance).clamp(-1.0, 1.0) as f32
    }
}

/// Stores a native pgvector embedding for a chunk row.
///
/// # Errors
/// Returns any `SQLx` execution error raised while updating the `chunk`.`embedding` column.
pub async fn set_chunk_embedding_vector(
    pool: &PgPool,
    chunk_id: Uuid,
    embedding: &[f32],
) -> Result<(), sqlx::Error> {
    sqlx::query("update chunk set embedding = $2 where id = $1")
        .bind(chunk_id)
        .bind(Vector::from(embedding.to_vec()))
        .execute(pool)
        .await?;
    Ok(())
}

/// Searches project chunks using the native pgvector embedding column.
///
/// # Errors
/// Returns any `SQLx` query error raised while loading scored `chunk` rows.
pub async fn search_chunks_by_project_embedding(
    pool: &PgPool,
    project_id: Uuid,
    embedding: &[f32],
    top_k: i32,
) -> Result<Vec<ScoredChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ScoredChunkRow>(
        "select
            id,
            document_id,
            project_id,
            ordinal,
            content,
            token_count,
            metadata_json,
            created_at,
            embedding <=> $2 as distance
         from chunk
         where project_id = $1
           and embedding is not null
         order by embedding <=> $2 asc, ordinal asc
         limit $3",
    )
    .bind(project_id)
    .bind(Vector::from(embedding.to_vec()))
    .bind(top_k)
    .fetch_all(pool)
    .await
}
