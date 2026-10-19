use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::types::RuntimeGraphFilteredArtifactRow;

/// Values required to persist one filtered runtime graph artifact.
///
/// The named payload keeps callers from passing the optional identifiers
/// (ingestion run, revision, source/target node key, relation type) positionally.
pub struct RuntimeGraphFilteredArtifactInput<'a> {
    pub library_id: Uuid,
    pub ingestion_run_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub target_kind: &'a str,
    pub candidate_key: &'a str,
    pub source_node_key: Option<&'a str>,
    pub target_node_key: Option<&'a str>,
    pub relation_type: Option<&'a str>,
    pub filter_reason: &'a str,
    pub summary: Option<&'a str>,
    pub metadata_json: serde_json::Value,
}

/// Persists one filtered graph artifact for later diagnostics.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the filtered artifact row.
pub async fn create_runtime_graph_filtered_artifact(
    pool: &PgPool,
    input: RuntimeGraphFilteredArtifactInput<'_>,
) -> Result<RuntimeGraphFilteredArtifactRow, sqlx::Error> {
    create_runtime_graph_filtered_artifact_with_executor(pool, input).await
}

pub async fn create_runtime_graph_filtered_artifact_with_executor<'e, E>(
    executor: E,
    input: RuntimeGraphFilteredArtifactInput<'_>,
) -> Result<RuntimeGraphFilteredArtifactRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "insert into runtime_graph_filtered_artifact (
            id, library_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         returning id, library_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.library_id)
    .bind(input.ingestion_run_id)
    .bind(input.revision_id)
    .bind(input.target_kind)
    .bind(input.candidate_key)
    .bind(input.source_node_key)
    .bind(input.target_node_key)
    .bind(input.relation_type)
    .bind(input.filter_reason)
    .bind(input.summary)
    .bind(input.metadata_json)
    .fetch_one(executor)
    .await
}
