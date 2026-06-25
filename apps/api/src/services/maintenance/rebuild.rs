//! `rebuild.*` operator-only sweepers.
//!
//! Heavy / cost-sensitive operations that must never run from the
//! background scheduler. Each entry point requires an explicit operator
//! invocation through `ironrag-maintenance rebuild …`.

use anyhow::Context;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ai::AiBindingPurpose, infra::repositories::catalog_repository,
    services::graph::error::GraphServiceError,
};

/// Reconcile per-dim vector relation dimensions with a source library's active
/// vector binding and rebuild all library vector material that must share those
/// indexes.
///
/// Wraps the canonical `search.rebuild_vector_plane_for_library` so the
/// CLI surface mirrors the in-process service call.
pub async fn vector_plane(state: &AppState, source_library_id: Uuid) -> anyhow::Result<()> {
    let outcome = state
        .canonical_services
        .search
        .rebuild_vector_plane_for_library(state, source_library_id)
        .await
        .with_context(|| {
            format!("failed to rebuild vector plane from library binding {source_library_id}",)
        })?;
    info!(
        library_id = %source_library_id,
        previous_dimensions = ?outcome.previous_dimensions,
        target_dimensions = outcome.target_dimensions,
        indexes_recreated = outcome.indexes_recreated,
        libraries_rebuilt = outcome.libraries_rebuilt,
        chunk_embeddings_rebuilt = outcome.chunk_embeddings_rebuilt,
        graph_node_embeddings_rebuilt = outcome.graph_node_embeddings_rebuilt,
        "vector-plane rebuild completed",
    );
    Ok(())
}

/// Re-run the canonical runtime-graph projection for one library, or for
/// every library when `library_filter` is `None`.
///
/// Batch mode tolerates `StateConflict` errors per-library (graph source
/// material inconsistent) and surfaces a non-zero exit at the end so an
/// operator script can detect partial completion without log grep.
pub async fn runtime_graph(state: &AppState, library_filter: Option<Uuid>) -> anyhow::Result<()> {
    let batch_mode = library_filter.is_none();
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched rebuild target");
    }

    let mut conflict_count = 0usize;
    for library in libraries {
        info!(
            library_id = %library.id,
            workspace_id = %library.workspace_id,
            library_name = %library.display_name,
            "rebuilding runtime graph"
        );
        let outcome =
            match state.canonical_services.graph.rebuild_library_graph(state, library.id).await {
                Ok(outcome) => outcome,
                Err(GraphServiceError::StateConflict { message }) if batch_mode => {
                    conflict_count = conflict_count.saturating_add(1);
                    warn!(
                        library_id = %library.id,
                        message = %message,
                        "runtime graph rebuild skipped library because graph state is inconsistent",
                    );
                    continue;
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error)).with_context(|| {
                        format!("failed to rebuild graph for library {}", library.id)
                    });
                }
            };
        info!(
            library_id = %library.id,
            projection_version = outcome.projection_version,
            node_count = outcome.node_count,
            edge_count = outcome.edge_count,
            "runtime graph rebuild completed",
        );
    }

    if conflict_count > 0 {
        anyhow::bail!(
            "runtime graph rebuild skipped {conflict_count} libraries because graph source material was inconsistent",
        );
    }
    Ok(())
}

/// Re-embed every entity node in `library_id` into the per-dim
/// `knowledge_entity_vector_d*` PostgreSQL relations.
///
/// Fails loudly if no active `EmbedChunk` binding is configured for the
/// library (binding=embed_chunk, reason=not_configured).  The underlying
/// `search.rebuild_graph_node_embeddings` upserts by
/// `(entity_id, model_catalog_id, freshness_generation)`, so the
/// operation is idempotent and safe to re-run.
pub async fn entity_embeddings(state: &AppState, library_id: Uuid) -> anyhow::Result<usize> {
    // Fail-loud guard: check the binding before the heavy rebuild path.
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .with_context(|| {
            format!("binding=embed_chunk, reason=not_configured, library_id={library_id}",)
        })?;
    if binding.is_none() {
        anyhow::bail!("binding=embed_chunk, reason=not_configured, library_id={library_id}",);
    }

    let vectors_upserted = state
        .canonical_services
        .search
        .rebuild_graph_node_embeddings(state, library_id)
        .await
        .with_context(|| format!("failed to rebuild entity embeddings for library {library_id}"))?;
    Ok(vectors_upserted)
}
