use pgvector::Vector;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::infra::repositories::{
    ChunkRow, RuntimeVectorTargetRow, list_runtime_vector_targets_by_project_and_kind,
};

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

#[derive(Debug, Clone)]
pub struct ScoredRuntimeVectorTargetRow {
    pub row: RuntimeVectorTargetRow,
    pub score: f32,
}

/// Searches canonical entity or relation embeddings for one project/provider tuple.
///
/// This currently computes cosine similarity in Rust from the persisted JSON embedding payload.
///
/// # Errors
/// Returns any `SQLx` query error raised while loading vector targets.
pub async fn search_runtime_vector_targets(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    provider_kind: &str,
    model_name: &str,
    embedding: &[f32],
    top_k: usize,
) -> Result<Vec<ScoredRuntimeVectorTargetRow>, sqlx::Error> {
    let mut rows = list_runtime_vector_targets_by_project_and_kind(
        pool,
        project_id,
        target_kind,
        provider_kind,
        model_name,
    )
    .await?
    .into_iter()
    .filter_map(|row| {
        let candidate = serde_json::from_value::<Vec<f32>>(row.embedding_json.clone()).ok()?;
        let score = cosine_similarity(embedding, &candidate)?;
        Some(ScoredRuntimeVectorTargetRow { row, score })
    })
    .collect::<Vec<_>>();

    rows.sort_by(|left, right| right.score.total_cmp(&left.score));
    rows.truncate(top_k);
    Ok(rows)
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.is_empty() || right.is_empty() || left.len() != right.len() {
        return None;
    }

    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (lhs, rhs) in left.iter().zip(right.iter()) {
        dot += lhs * rhs;
        left_norm += lhs * lhs;
        right_norm += rhs * rhs;
    }

    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator <= f32::EPSILON {
        return None;
    }

    Some(dot / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_rejects_shape_mismatch() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), None);
    }

    #[test]
    fn cosine_similarity_scores_identical_vectors_highest() {
        let identical = cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).expect("identical score");
        let orthogonal = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).expect("orthogonal score");

        assert!(identical > orthogonal);
        assert!((identical - 1.0).abs() < f32::EPSILON);
    }
}
