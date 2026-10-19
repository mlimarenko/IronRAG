use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::VectorPlaneDeleteOutcome,
    postgres::pg_search_store::delete_entity_vectors_by_library_fenced_in_transaction,
};

/// Atomically removes a library's materialized graph and every derived entity
/// vector. The vector mutation acquires the cross-process exclusive advisory
/// lock and catalog row lock before either projection is changed, so a deferred
/// writer must commit before this sweep or revalidate after the sources vanish.
pub async fn reset_library_materialized_graph_and_vectors(
    postgres: &PgPool,
    library_id: Uuid,
    expected_source_truth_version: i64,
) -> anyhow::Result<VectorPlaneDeleteOutcome> {
    let mut transaction =
        postgres.begin().await.context("begin atomic graph materialization reset")?;
    let vector_outcome = delete_entity_vectors_by_library_fenced_in_transaction(
        &mut transaction,
        library_id,
        expected_source_truth_version,
    )
    .await
    .context("delete entity vectors under graph materialization reset fence")?;

    for statement in [
        "delete from knowledge_chunk_entity_mention where library_id = $1",
        "delete from knowledge_evidence_entity_support where library_id = $1",
        "delete from knowledge_evidence_relation_support where library_id = $1",
        "delete from knowledge_bundle_entity where library_id = $1",
        "delete from knowledge_bundle_relation where library_id = $1",
        "delete from knowledge_bundle_evidence where library_id = $1",
        "delete from knowledge_evidence where library_id = $1",
        "delete from knowledge_relation where library_id = $1",
        "delete from knowledge_entity where library_id = $1",
        "delete from runtime_graph_evidence where library_id = $1",
        "delete from runtime_graph_edge where library_id = $1",
        "delete from runtime_graph_node where library_id = $1",
        "delete from runtime_graph_community where library_id = $1",
        "delete from runtime_graph_canonical_summary where library_id = $1",
    ] {
        sqlx::query(statement)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("failed to run graph reset statement `{statement}`"))?;
    }

    transaction.commit().await.context("commit atomic graph materialization reset")?;
    Ok(vector_outcome)
}
