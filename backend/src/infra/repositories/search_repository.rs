use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct SearchChunkEmbeddingRow {
    pub chunk_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Option<Vector>,
    pub embedded_at: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct SearchGraphNodeEmbeddingRow {
    pub node_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Option<Vector>,
    pub embedded_at: DateTime<Utc>,
    pub active: bool,
}

pub async fn upsert_chunk_embedding(
    pool: &PgPool,
    chunk_id: Uuid,
    model_catalog_id: Uuid,
    embedding_vector: Option<&[f32]>,
    active: bool,
) -> Result<SearchChunkEmbeddingRow, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "insert into search_chunk_embedding (
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
        )
        values ($1, $2, $3, now(), $4)
        on conflict (chunk_id, model_catalog_id)
        do update set
            embedding_vector = excluded.embedding_vector,
            embedded_at = now(),
            active = excluded.active
        returning
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(chunk_id)
    .bind(model_catalog_id)
    .bind(embedding_vector.map(|embedding| Vector::from(embedding.to_vec())))
    .bind(active)
    .fetch_one(pool)
    .await
}

pub async fn get_chunk_embedding(
    pool: &PgPool,
    chunk_id: Uuid,
    model_catalog_id: Uuid,
) -> Result<Option<SearchChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "select
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_chunk_embedding
         where chunk_id = $1 and model_catalog_id = $2",
    )
    .bind(chunk_id)
    .bind(model_catalog_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_chunk_embeddings_by_chunk(
    pool: &PgPool,
    chunk_id: Uuid,
) -> Result<Vec<SearchChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "select
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_chunk_embedding
         where chunk_id = $1
         order by embedded_at desc",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
}

pub async fn list_active_chunk_embeddings_by_chunk(
    pool: &PgPool,
    chunk_id: Uuid,
) -> Result<Vec<SearchChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "select
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_chunk_embedding
         where chunk_id = $1
           and active = true
         order by embedded_at desc",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
}

pub async fn set_chunk_embedding_active(
    pool: &PgPool,
    chunk_id: Uuid,
    model_catalog_id: Uuid,
    active: bool,
) -> Result<Option<SearchChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "update search_chunk_embedding
         set active = $3,
             embedded_at = now()
         where chunk_id = $1 and model_catalog_id = $2
         returning
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(chunk_id)
    .bind(model_catalog_id)
    .bind(active)
    .fetch_optional(pool)
    .await
}

pub async fn delete_chunk_embedding(
    pool: &PgPool,
    chunk_id: Uuid,
    model_catalog_id: Uuid,
) -> Result<Option<SearchChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchChunkEmbeddingRow>(
        "delete from search_chunk_embedding
         where chunk_id = $1 and model_catalog_id = $2
         returning
            chunk_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(chunk_id)
    .bind(model_catalog_id)
    .fetch_optional(pool)
    .await
}

pub async fn upsert_graph_node_embedding(
    pool: &PgPool,
    node_id: Uuid,
    model_catalog_id: Uuid,
    embedding_vector: Option<&[f32]>,
    active: bool,
) -> Result<SearchGraphNodeEmbeddingRow, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "insert into search_graph_node_embedding (
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
        )
        values ($1, $2, $3, now(), $4)
        on conflict (node_id, model_catalog_id)
        do update set
            embedding_vector = excluded.embedding_vector,
            embedded_at = now(),
            active = excluded.active
        returning
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(node_id)
    .bind(model_catalog_id)
    .bind(embedding_vector.map(|embedding| Vector::from(embedding.to_vec())))
    .bind(active)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_node_embedding(
    pool: &PgPool,
    node_id: Uuid,
    model_catalog_id: Uuid,
) -> Result<Option<SearchGraphNodeEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "select
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_graph_node_embedding
         where node_id = $1 and model_catalog_id = $2",
    )
    .bind(node_id)
    .bind(model_catalog_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_node_embeddings_by_node(
    pool: &PgPool,
    node_id: Uuid,
) -> Result<Vec<SearchGraphNodeEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "select
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_graph_node_embedding
         where node_id = $1
         order by embedded_at desc",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await
}

pub async fn list_active_graph_node_embeddings_by_node(
    pool: &PgPool,
    node_id: Uuid,
) -> Result<Vec<SearchGraphNodeEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "select
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active
         from search_graph_node_embedding
         where node_id = $1
           and active = true
         order by embedded_at desc",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await
}

pub async fn set_graph_node_embedding_active(
    pool: &PgPool,
    node_id: Uuid,
    model_catalog_id: Uuid,
    active: bool,
) -> Result<Option<SearchGraphNodeEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "update search_graph_node_embedding
         set active = $3,
             embedded_at = now()
         where node_id = $1 and model_catalog_id = $2
         returning
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(node_id)
    .bind(model_catalog_id)
    .bind(active)
    .fetch_optional(pool)
    .await
}

pub async fn delete_graph_node_embedding(
    pool: &PgPool,
    node_id: Uuid,
    model_catalog_id: Uuid,
) -> Result<Option<SearchGraphNodeEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, SearchGraphNodeEmbeddingRow>(
        "delete from search_graph_node_embedding
         where node_id = $1 and model_catalog_id = $2
         returning
            node_id,
            model_catalog_id,
            embedding_vector,
            embedded_at,
            active",
    )
    .bind(node_id)
    .bind(model_catalog_id)
    .fetch_optional(pool)
    .await
}
