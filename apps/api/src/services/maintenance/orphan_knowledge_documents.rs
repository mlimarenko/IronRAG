//! `repair orphan-knowledge-documents` — purge knowledge-plane documents whose
//! canonical `content_document` row no longer exists.
//!
//! The strict readable-content parity gate compares canonical content rows
//! against their knowledge-plane projection before every query. A knowledge
//! document that lost its canonical counterpart (pre-hardening deletes removed
//! only the canonical side) can never converge, so one orphan permanently
//! wedges its whole library in `query_content_projection_converging`.
//!
//! Knowledge-side children (revisions, chunks, mentions, bundles, facts)
//! cascade from `knowledge_document`. Chunk vectors live in dynamic per-dim
//! relations without foreign keys and `runtime_graph_evidence` references
//! documents by plain columns, so both are swept explicitly in the same
//! transaction. The library source-truth generation is advanced so every
//! process re-evaluates parity instead of trusting its per-generation cache,
//! and the runtime graph is re-projected afterwards so node/edge support
//! counts stop counting evidence from purged documents. Idempotent.

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::catalog_repository};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct OrphanKnowledgeDocumentReport {
    pub libraries_repaired: usize,
    pub orphan_documents_removed: u64,
    pub chunk_vector_rows_removed: u64,
    pub graph_evidence_rows_removed: u64,
    pub graph_rebuilds_completed: usize,
    pub graph_rebuilds_skipped: usize,
}

/// Remove orphaned knowledge-plane documents for one library or every library.
pub async fn orphan_knowledge_documents(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> Result<OrphanKnowledgeDocumentReport> {
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched orphan-knowledge-document repair target");
    }

    let mut report = OrphanKnowledgeDocumentReport::default();
    for library in libraries {
        let orphan_ids: Vec<Uuid> = sqlx::query_scalar(
            "select k.document_id from knowledge_document k
             where k.library_id = $1
               and not exists (select 1 from content_document cd where cd.id = k.document_id)",
        )
        .bind(library.id)
        .fetch_all(&state.persistence.postgres)
        .await
        .context("failed to list orphan knowledge documents")?;
        if orphan_ids.is_empty() {
            continue;
        }

        if dry_run {
            report.libraries_repaired += 1;
            report.orphan_documents_removed += orphan_ids.len() as u64;
            info!(
                library_id = %library.id,
                orphan_documents = orphan_ids.len(),
                "dry-run: orphan knowledge documents would be removed",
            );
            continue;
        }

        let removed = purge_library_orphans(state, library.workspace_id, library.id, &orphan_ids)
            .await
            .with_context(|| {
                format!("failed to purge orphan knowledge documents for library {}", library.id)
            })?;
        report.libraries_repaired += 1;
        report.orphan_documents_removed += removed.orphan_documents;
        report.chunk_vector_rows_removed += removed.chunk_vector_rows;
        report.graph_evidence_rows_removed += removed.graph_evidence_rows;
        info!(
            library_id = %library.id,
            orphan_documents = removed.orphan_documents,
            chunk_vector_rows = removed.chunk_vector_rows,
            graph_evidence_rows = removed.graph_evidence_rows,
            "orphan knowledge documents removed",
        );

        match state.canonical_services.graph.rebuild_library_graph(state, library.id).await {
            Ok(outcome) => {
                report.graph_rebuilds_completed += 1;
                info!(
                    library_id = %library.id,
                    projection_version = outcome.projection_version,
                    node_count = outcome.node_count,
                    edge_count = outcome.edge_count,
                    "runtime graph re-projected after orphan purge",
                );
            }
            Err(error) => {
                report.graph_rebuilds_skipped += 1;
                warn!(
                    library_id = %library.id,
                    ?error,
                    "runtime graph re-projection failed after orphan purge; \
                     run `rebuild runtime-graph --library` once graph state settles",
                );
            }
        }
    }
    Ok(report)
}

struct PurgedCounts {
    orphan_documents: u64,
    chunk_vector_rows: u64,
    graph_evidence_rows: u64,
}

async fn purge_library_orphans(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    orphan_ids: &[Uuid],
) -> Result<PurgedCounts> {
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .context("failed to start orphan-knowledge-document transaction")?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await
    .context("failed to lock library for orphan purge")?;
    anyhow::ensure!(parent_locked, "library {library_id} disappeared during orphan purge");

    let chunk_ids: Vec<Uuid> = sqlx::query_scalar(
        "select chunk_id from knowledge_chunk
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(orphan_ids)
    .fetch_all(&mut *transaction)
    .await
    .context("failed to list orphan chunk ids")?;

    let mut chunk_vector_rows = 0_u64;
    if !chunk_ids.is_empty() {
        let relations: Vec<String> = sqlx::query_scalar(
            "select distinct relation_name from knowledge_vector_relation_manifest
             where library_id = $1 and relation_name like 'knowledge\\_chunk\\_vector\\_d%'",
        )
        .bind(library_id)
        .fetch_all(&mut *transaction)
        .await
        .context("failed to list chunk vector relations")?;
        for relation_name in &relations {
            let relation = validated_chunk_relation_identifier(relation_name)?;
            chunk_vector_rows += sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {relation} where library_id = $1 and chunk_id = any($2)",
            )))
            .bind(library_id)
            .bind(&chunk_ids)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("failed to delete orphan chunk vectors from {relation}"))?
            .rows_affected();
        }
    }

    let graph_evidence_rows = sqlx::query(
        "delete from runtime_graph_evidence
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(orphan_ids)
    .execute(&mut *transaction)
    .await
    .context("failed to delete orphan graph evidence")?
    .rows_affected();

    let orphan_documents = sqlx::query(
        "delete from knowledge_document
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(orphan_ids)
    .execute(&mut *transaction)
    .await
    .context("failed to delete orphan knowledge documents")?
    .rows_affected();

    catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await
    .context("failed to advance library source truth version after orphan purge")?;

    transaction.commit().await.context("failed to commit orphan-knowledge-document purge")?;
    Ok(PurgedCounts { orphan_documents, chunk_vector_rows, graph_evidence_rows })
}

/// The relation name is interpolated into SQL; accept only per-dim chunk
/// vector relations (`knowledge_chunk_vector_d` + decimal digits).
fn validated_chunk_relation_identifier(relation_name: &str) -> Result<&str> {
    let digits = relation_name
        .strip_prefix("knowledge_chunk_vector_d")
        .with_context(|| format!("relation {relation_name} is not a chunk vector relation"))?;
    anyhow::ensure!(
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        "relation {relation_name} does not end in a plain decimal dimension"
    );
    Ok(relation_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_chunk_vector_relations() {
        assert!(validated_chunk_relation_identifier("knowledge_chunk_vector_d3072").is_ok());
        assert!(validated_chunk_relation_identifier("knowledge_entity_vector_d3072").is_err());
        assert!(validated_chunk_relation_identifier("knowledge_chunk_vector_d").is_err());
        assert!(validated_chunk_relation_identifier("knowledge_chunk_vector_d1; drop x").is_err());
    }
}
