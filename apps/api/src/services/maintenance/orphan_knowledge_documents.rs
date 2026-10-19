//! `repair orphan-knowledge-documents` — purge knowledge-plane documents whose
//! stored `content_document` row no longer exists.
//!
//! A knowledge document that lost its stored counterpart remains readable but
//! stale. Queries continue from the readable snapshot and expose the drift as
//! a catching-up warning until this repair removes the orphan.
//!
//! The removal itself is
//! `content_knowledge_projection_repository::purge_knowledge_projection_with_transaction`,
//! shared with the external-key reclaim so both sweep the same non-cascading
//! rows. On top of it this repair advances the library source-truth generation
//! so every process re-evaluates parity instead of trusting its per-generation
//! cache, and re-projects the runtime graph afterwards so node/edge support
//! counts stop counting evidence from purged documents. Idempotent.

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{catalog_repository, content_knowledge_projection_repository},
};

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
        let orphan_ids = list_orphan_document_ids(state, library.id).await?;
        if orphan_ids.is_empty() {
            continue;
        }
        if dry_run {
            record_dry_run_orphans(&mut report, &library, orphan_ids.len());
            continue;
        }
        purge_library_orphans_and_reproject(state, &library, &orphan_ids, &mut report).await?;
    }
    Ok(report)
}

/// Knowledge-plane documents in `library_id` whose stored `content_document`
/// counterpart is gone.
async fn list_orphan_document_ids(state: &AppState, library_id: Uuid) -> Result<Vec<Uuid>> {
    sqlx::query_scalar::<_, Uuid>(
        "select k.document_id from knowledge_document k
         where k.library_id = $1
           and not exists (select 1 from content_document cd where cd.id = k.document_id)",
    )
    .bind(library_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .context("failed to list orphan knowledge documents")
}

fn record_dry_run_orphans(
    report: &mut OrphanKnowledgeDocumentReport,
    library: &catalog_repository::CatalogLibraryRow,
    orphan_documents: usize,
) {
    report.libraries_repaired += 1;
    report.orphan_documents_removed += orphan_documents as u64;
    info!(
        library_id = %library.id,
        orphan_documents,
        "dry-run: orphan knowledge documents would be removed",
    );
}

/// Purge one library's orphans and re-project its runtime graph, accumulating
/// the counters into `report`.
async fn purge_library_orphans_and_reproject(
    state: &AppState,
    library: &catalog_repository::CatalogLibraryRow,
    orphan_ids: &[Uuid],
    report: &mut OrphanKnowledgeDocumentReport,
) -> Result<()> {
    let removed = purge_library_orphans(state, library.workspace_id, library.id, orphan_ids)
        .await
        .with_context(|| {
            format!("failed to purge orphan knowledge documents for library {}", library.id)
        })?;
    report.libraries_repaired += 1;
    report.orphan_documents_removed += removed.documents;
    report.chunk_vector_rows_removed += removed.chunk_vector_rows;
    report.graph_evidence_rows_removed += removed.graph_evidence_rows;
    info!(
        library_id = %library.id,
        orphan_documents = removed.documents,
        chunk_vector_rows = removed.chunk_vector_rows,
        graph_evidence_rows = removed.graph_evidence_rows,
        "orphan knowledge documents removed",
    );

    match state.application_services.graph.rebuild_library_graph(state, library.id).await {
        Ok(outcome) => {
            report.graph_rebuilds_completed += 1;
            log_runtime_graph_reprojected(
                library.id,
                outcome.projection_version,
                outcome.node_count,
                outcome.edge_count,
            );
        }
        Err(error) => {
            report.graph_rebuilds_skipped += 1;
            log_runtime_graph_reproject_failed(library.id, &error);
        }
    }
    Ok(())
}

fn log_runtime_graph_reprojected(
    library_id: Uuid,
    projection_version: i64,
    node_count: usize,
    edge_count: usize,
) {
    info!(
        library_id = %library_id,
        projection_version,
        node_count,
        edge_count,
        "runtime graph re-projected after orphan purge",
    );
}

fn log_runtime_graph_reproject_failed<E: std::fmt::Debug>(library_id: Uuid, error: &E) {
    warn!(
        library_id = %library_id,
        ?error,
        "runtime graph re-projection failed after orphan purge; \
         run `rebuild runtime-graph --library` once graph state settles",
    );
}

async fn purge_library_orphans(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    orphan_ids: &[Uuid],
) -> Result<content_knowledge_projection_repository::PurgedKnowledgeProjection> {
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

    let purged =
        content_knowledge_projection_repository::purge_knowledge_projection_with_transaction(
            &mut transaction,
            library_id,
            orphan_ids,
        )
        .await
        .context("failed to purge orphan knowledge documents")?;

    catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await
    .context("failed to advance library source truth version after orphan purge")?;

    transaction.commit().await.context("failed to commit orphan-knowledge-document purge")?;
    Ok(purged)
}
