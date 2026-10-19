use std::collections::BTreeSet;

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::revision_text_state_is_readable,
    infra::{
        knowledge_rows::{NewKnowledgeEntityCandidate, NewKnowledgeRelationCandidate},
        repositories,
    },
    services::graph::service::{
        GraphService, MaterializedExtractCandidates, build_materialized_extract_candidates,
    },
};

impl GraphService {
    pub(in crate::services::graph::service) async fn refresh_library_candidate_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let library = state
            .application_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for knowledge candidate rebuild")?;
        let documents = state
            .document_store
            .list_documents_by_library(library.workspace_id, library_id, false)
            .await
            .context("failed to list documents for knowledge candidate rebuild")?;
        let revision_ids = documents
            .iter()
            .filter(|document| document.deleted_at.is_none())
            .flat_map(|document| [document.readable_revision_id, document.active_revision_id])
            .flatten()
            .collect::<BTreeSet<_>>();

        let mut entity_candidates = Vec::<NewKnowledgeEntityCandidate>::new();
        let mut relation_candidates = Vec::<NewKnowledgeRelationCandidate>::new();
        for revision_id in revision_ids {
            let Some(revision) =
                state.document_store.get_revision(revision_id).await.with_context(|| {
                    format!("failed to load knowledge revision {revision_id} for candidate rebuild")
                })?
            else {
                continue;
            };
            if revision.superseded_by_revision_id.is_some()
                || !revision_text_state_is_readable(&revision.text_state)
            {
                continue;
            }

            let materialized =
                self.load_revision_materialized_extract_candidates(state, &revision).await?;
            entity_candidates.extend(materialized.entity_candidates);
            relation_candidates.extend(materialized.relation_candidates);
        }

        state
            .graph_store
            .delete_entity_candidates_by_library(library_id)
            .await
            .context("failed to clear stale knowledge entity candidates before rebuild")?;
        state
            .graph_store
            .delete_relation_candidates_by_library(library_id)
            .await
            .context("failed to clear stale knowledge relation candidates before rebuild")?;

        for batch in entity_candidates.chunks(256) {
            state.graph_store.upsert_entity_candidates(batch).await.with_context(|| {
                format!(
                    "failed to persist {} knowledge entity candidates during rebuild",
                    batch.len()
                )
            })?;
        }

        for batch in relation_candidates.chunks(256) {
            state.graph_store.upsert_relation_candidates(batch).await.with_context(|| {
                format!(
                    "failed to persist {} knowledge relation candidates during rebuild",
                    batch.len()
                )
            })?;
        }

        Ok(())
    }

    async fn load_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    ) -> Result<MaterializedExtractCandidates> {
        let chunk_results =
            repositories::extract_repository::list_ready_extract_chunk_results_by_revision(
                &state.persistence.postgres,
                revision.revision_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load normalized extract chunk results for revision {}",
                    revision.revision_id
                )
            })?;

        self.collect_revision_materialized_extract_candidates(state, revision, &chunk_results).await
    }

    async fn collect_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
        chunk_results: &[repositories::extract_repository::ExtractChunkResultRow],
    ) -> Result<MaterializedExtractCandidates> {
        let mut materialized = MaterializedExtractCandidates::default();
        for chunk_result in chunk_results {
            let node_candidates =
                repositories::extract_repository::list_extract_node_candidates_by_chunk_result(
                    &state.persistence.postgres,
                    chunk_result.id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load normalized extract node candidates for chunk result {}",
                        chunk_result.id
                    )
                })?;
            let edge_candidates =
                repositories::extract_repository::list_extract_edge_candidates_by_chunk_result(
                    &state.persistence.postgres,
                    chunk_result.id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load normalized extract edge candidates for chunk result {}",
                        chunk_result.id
                    )
                })?;
            let chunk_materialized = build_materialized_extract_candidates(
                revision,
                chunk_result,
                &node_candidates,
                &edge_candidates,
            );
            materialized.entity_candidates.extend(chunk_materialized.entity_candidates);
            materialized.relation_candidates.extend(chunk_materialized.relation_candidates);
        }
        Ok(materialized)
    }
}
