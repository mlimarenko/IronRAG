use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

mod canonicalization;
mod evidence;
mod materialization;
#[cfg(test)]
mod tests;
mod upserts;

use crate::{
    app::state::AppState,
    domains::content::revision_text_state_is_readable,
    infra::{
        arangodb::graph_store::{
            GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, sanitize_graph_view_writes,
        },
        repositories,
    },
    services::{
        graph::extract::GraphExtractionCandidateSet,
        graph::merge::{GraphMergeOutcome, GraphMergeScope},
        graph::projection::{GraphProjectionOutcome, GraphProjectionScope},
        graph::rebuild::RevisionGraphReconcileOutcome,
        graph::summary::{GraphSummaryRefreshRequest, GraphSummaryService},
    },
};

use canonicalization::{
    MaterializedExtractCandidates, ReconciledEntityCandidate, ReconciledRelationCandidate,
    apply_entity_key_aliases_to_relation_candidate, build_entity_candidate_key_index,
    build_materialized_extract_candidates, build_prefixed_entity_key_aliases,
    build_relation_entity_key_index, canonical_chunk_mentions_entity_edge_key,
    canonical_document_revision_edge_key, canonical_edge_relation_key,
    canonical_entity_candidate_id, canonical_entity_id, canonical_evidence_id,
    canonical_relation_assertion_from_keys, canonical_relation_candidate_id, canonical_relation_id,
    canonical_revision_chunk_edge_key, placeholder_entity_parts_from_key,
    reconcile_entity_candidate_row, reconcile_relation_candidate_row,
    relation_candidate_keys_are_materializable, select_canonical_entity_label,
};
#[cfg(test)]
use canonicalization::{canonical_entity_normalization_key, canonical_relation_assertion};
#[cfg(test)]
use evidence::resolve_entity_evidence_support;
use evidence::{normalize_evidence_literal, relation_fields_are_semantically_empty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArangoGraphRebuildTarget {
    Text,
    Vector,
    Graph,
    Evidence,
    Library,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArangoGraphRebuildOutcome {
    pub target: Option<ArangoGraphRebuildTarget>,
    pub scanned_entity_candidates: usize,
    pub scanned_relation_candidates: usize,
    pub upserted_entities: usize,
    pub upserted_relations: usize,
    pub upserted_evidence: usize,
    pub upserted_document_revision_edges: usize,
    pub upserted_revision_chunk_edges: usize,
    pub upserted_chunk_entity_edges: usize,
    pub upserted_relation_subject_edges: usize,
    pub upserted_relation_object_edges: usize,
    pub upserted_evidence_source_edges: usize,
    pub upserted_evidence_support_entity_edges: usize,
    pub upserted_evidence_support_relation_edges: usize,
    pub stale_evidence_marked: usize,
    pub text_reconciled_revisions: usize,
    pub chunk_embeddings_rebuilt: usize,
    pub graph_node_embeddings_rebuilt: usize,
}

impl ArangoGraphRebuildOutcome {
    #[must_use]
    pub const fn has_materialized_graph(&self) -> bool {
        self.upserted_entities > 0 || self.upserted_relations > 0 || self.upserted_evidence > 0
    }
}

/// Per-library mutex registry that serializes graph merge work for the same
/// library while keeping different libraries fully parallel. The merge phase
/// of the ingest pipeline upserts overlapping `runtime_graph_evidence` rows
/// keyed by `(library_id, evidence_identity_key)`. Without per-library
/// serialization, parallel workers contend on row-level locks and the latency
/// of every UPSERT in the same library grows quadratically with concurrency.
#[derive(Clone, Default)]
pub struct GraphService {
    library_merge_locks: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    >,
}

impl GraphService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn library_merge_lock(&self, library_id: Uuid) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut guard =
            self.library_merge_locks.lock().expect("library merge lock registry poisoned");
        guard
            .entry(library_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    #[must_use]
    pub fn merge_projection_data(
        current: &GraphViewData,
        incoming: &GraphViewData,
    ) -> GraphViewData {
        let mut nodes = BTreeMap::<String, GraphViewNodeWrite>::new();
        for node in current.nodes.iter().chain(incoming.nodes.iter()) {
            nodes.insert(node.canonical_key.clone(), node.clone());
        }

        let mut edges = BTreeMap::<String, GraphViewEdgeWrite>::new();
        for edge in current.edges.iter().chain(incoming.edges.iter()) {
            edges.insert(edge.canonical_key.clone(), edge.clone());
        }

        let merged = GraphViewData {
            nodes: nodes.into_values().collect(),
            edges: edges.into_values().collect(),
        };
        let (nodes, edges, _) = sanitize_graph_view_writes(&merged.nodes, &merged.edges);
        GraphViewData { nodes, edges }
    }

    pub async fn merge_chunk_graph_candidates(
        &self,
        pool: &sqlx::PgPool,
        graph_quality_guard: &crate::services::graph::quality_guard::GraphQualityGuardService,
        scope: &GraphMergeScope,
        document: &repositories::DocumentRow,
        chunk: &repositories::ChunkRow,
        candidates: &crate::services::graph::extract::GraphExtractionCandidateSet,
        extraction_recovery: Option<&crate::domains::graph_quality::ExtractionRecoverySummary>,
    ) -> Result<GraphMergeOutcome> {
        crate::services::graph::merge::merge_chunk_graph_candidates(
            pool,
            graph_quality_guard,
            scope,
            document,
            chunk,
            candidates,
            extraction_recovery,
        )
        .await
    }

    pub async fn merge_arango_graph_candidates(
        &self,
        state: &AppState,
        revision_id: Uuid,
        chunk_id: Uuid,
        candidates: &GraphExtractionCandidateSet,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .context("failed to load knowledge revision for arango graph merge")?
            .ok_or_else(|| anyhow::anyhow!("knowledge_revision {revision_id} not found"))?;

        self.materialize_current_candidate_batch(state, &revision, chunk_id, candidates, true)
            .await
            .with_context(|| {
                format!(
                    "failed to materialize arango graph candidates for revision {}",
                    revision_id
                )
            })?;

        let mut outcome = self
            .build_and_refresh_arango_graph_from_candidates(state, revision.library_id, None)
            .await?;
        outcome.target = Some(ArangoGraphRebuildTarget::Graph);
        self.recalculate_arango_library_generations(state, revision.library_id)
            .await
            .context("failed to refresh arango generation state after graph merge")?;
        Ok(outcome)
    }

    pub async fn invalidate_arango_revision_graph_artifacts(
        &self,
        state: &AppState,
        revision_id: Uuid,
        superseded_by_revision_id: Option<Uuid>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .context("failed to load knowledge revision for arango graph invalidation")?
            .ok_or_else(|| anyhow::anyhow!("knowledge_revision {revision_id} not found"))?;

        let stale_evidence = state
            .arango_graph_store
            .list_evidence_by_revision(revision_id)
            .await
            .context("failed to load arango evidence rows for invalidation")?;
        let mut marked_stale = 0usize;
        for evidence in stale_evidence {
            let _ = state
                .arango_graph_store
                .upsert_evidence(&crate::infra::arangodb::graph_store::NewKnowledgeEvidence {
                    evidence_id: evidence.evidence_id,
                    workspace_id: evidence.workspace_id,
                    library_id: evidence.library_id,
                    document_id: evidence.document_id,
                    revision_id: evidence.revision_id,
                    chunk_id: evidence.chunk_id,
                    block_id: evidence.block_id,
                    fact_id: evidence.fact_id,
                    span_start: evidence.span_start,
                    span_end: evidence.span_end,
                    quote_text: evidence.quote_text,
                    literal_spans_json: evidence.literal_spans_json,
                    evidence_kind: evidence.evidence_kind,
                    extraction_method: evidence.extraction_method,
                    confidence: evidence.confidence,
                    evidence_state: "superseded".to_string(),
                    freshness_generation: evidence.freshness_generation,
                    created_at: Some(evidence.created_at),
                    updated_at: Some(Utc::now()),
                })
                .await
                .context("failed to supersede stale arango evidence")?;
            marked_stale += 1;
        }

        let _ = state
            .arango_document_store
            .update_revision_readiness(
                revision_id,
                &revision.text_state,
                &revision.vector_state,
                &revision.graph_state,
                revision.text_readable_at,
                revision.vector_ready_at,
                revision.graph_ready_at,
                superseded_by_revision_id,
            )
            .await
            .context("failed to mark knowledge revision as superseded")?;

        let _ = state
            .arango_graph_store
            .delete_entity_candidates_by_revision(revision_id)
            .await
            .context("failed to delete stale entity candidates")?;
        let _ = state
            .arango_graph_store
            .delete_relation_candidates_by_revision(revision_id)
            .await
            .context("failed to delete stale relation candidates")?;

        let mut outcome =
            self.reconcile_arango_library_candidates(state, revision.library_id, None).await?;
        outcome.stale_evidence_marked += marked_stale;
        outcome.target = Some(ArangoGraphRebuildTarget::Evidence);
        self.recalculate_arango_library_generations(state, revision.library_id)
            .await
            .context("failed to refresh arango generation state after graph invalidation")?;
        Ok(outcome)
    }

    pub async fn rebuild_arango_library_text(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let library = state
            .canonical_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for arango text rebuild")?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id, false)
            .await
            .context("failed to list documents for arango text rebuild")?;
        let mut reconciled_revisions = 0usize;
        for document in documents {
            let revisions = state
                .arango_document_store
                .list_revisions_by_document(document.document_id)
                .await
                .context("failed to list revisions for arango text rebuild")?;
            for revision in revisions {
                if revision_text_state_is_readable(&revision.text_state) {
                    continue;
                }
                let chunks = state
                    .arango_document_store
                    .list_chunks_by_revision(revision.revision_id)
                    .await
                    .context("failed to list chunks for arango text rebuild")?;
                if chunks.is_empty() {
                    continue;
                }
                let _ = state
                    .canonical_services
                    .knowledge
                    .set_revision_text_state(
                        state,
                        revision.revision_id,
                        "readable",
                        None,
                        None,
                        Some(Utc::now()),
                    )
                    .await
                    .context("failed to reconcile arango text readiness")?;
                reconciled_revisions += 1;
            }
        }

        Ok(ArangoGraphRebuildOutcome {
            target: Some(ArangoGraphRebuildTarget::Text),
            text_reconciled_revisions: reconciled_revisions,
            ..Default::default()
        })
    }

    pub async fn rebuild_arango_library_vector(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let chunk_embeddings =
            state.canonical_services.search.rebuild_chunk_embeddings(state, library_id).await?;
        let graph_node_embeddings = state
            .canonical_services
            .search
            .rebuild_graph_node_embeddings(state, library_id)
            .await?;
        Ok(ArangoGraphRebuildOutcome {
            target: Some(ArangoGraphRebuildTarget::Vector),
            chunk_embeddings_rebuilt: chunk_embeddings,
            graph_node_embeddings_rebuilt: graph_node_embeddings,
            ..Default::default()
        })
    }

    pub async fn reconcile_arango_library_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.with_runtime_graph_lock(state, library_id, async {
            let mut outcome = self
                .build_and_refresh_arango_graph_from_candidates(state, library_id, None)
                .await?;
            outcome.target = Some(ArangoGraphRebuildTarget::Graph);
            self.recalculate_arango_library_generations(state, library_id)
                .await
                .context("failed to refresh arango generation state after graph reconcile")?;
            Ok(outcome)
        })
        .await
    }

    pub async fn rebuild_arango_library_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.with_runtime_graph_lock(state, library_id, async {
            self.refresh_arango_library_candidate_materialization(state, library_id).await?;
            let mut outcome =
                self.reconcile_arango_library_candidates(state, library_id, None).await?;
            outcome.target = Some(ArangoGraphRebuildTarget::Evidence);
            self.recalculate_arango_library_generations(state, library_id)
                .await
                .context("failed to refresh arango generation state after evidence rebuild")?;
            Ok(outcome)
        })
        .await
    }

    pub async fn rebuild_arango_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.with_runtime_graph_lock(state, library_id, async {
            let text = self.rebuild_arango_library_text(state, library_id).await?;
            self.refresh_arango_library_candidate_materialization(state, library_id).await?;
            let graph = self.reconcile_arango_library_candidates(state, library_id, None).await?;
            let vector = self.rebuild_arango_library_vector(state, library_id).await?;
            let mut outcome = ArangoGraphRebuildOutcome {
                target: Some(ArangoGraphRebuildTarget::Library),
                ..Default::default()
            };
            outcome.text_reconciled_revisions = text.text_reconciled_revisions;
            outcome.chunk_embeddings_rebuilt = vector.chunk_embeddings_rebuilt;
            outcome.graph_node_embeddings_rebuilt = vector.graph_node_embeddings_rebuilt;
            outcome.scanned_entity_candidates = graph.scanned_entity_candidates;
            outcome.scanned_relation_candidates = graph.scanned_relation_candidates;
            outcome.upserted_entities = graph.upserted_entities;
            outcome.upserted_relations = graph.upserted_relations;
            outcome.upserted_evidence = graph.upserted_evidence;
            outcome.upserted_document_revision_edges = graph.upserted_document_revision_edges;
            outcome.upserted_revision_chunk_edges = graph.upserted_revision_chunk_edges;
            outcome.upserted_chunk_entity_edges = graph.upserted_chunk_entity_edges;
            outcome.upserted_relation_subject_edges = graph.upserted_relation_subject_edges;
            outcome.upserted_relation_object_edges = graph.upserted_relation_object_edges;
            outcome.upserted_evidence_source_edges = graph.upserted_evidence_source_edges;
            outcome.upserted_evidence_support_entity_edges =
                graph.upserted_evidence_support_entity_edges;
            outcome.upserted_evidence_support_relation_edges =
                graph.upserted_evidence_support_relation_edges;
            outcome.stale_evidence_marked = graph.stale_evidence_marked;
            self.recalculate_arango_library_generations(state, library_id)
                .await
                .context("failed to refresh arango generation state after library rebuild")?;
            Ok(outcome)
        })
        .await
    }

    async fn with_runtime_graph_lock<T, F>(
        &self,
        state: &AppState,
        library_id: Uuid,
        operation: F,
    ) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        let graph_lock = repositories::acquire_runtime_library_graph_lock(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .context("failed to acquire canonical graph advisory lock")?;
        let result = operation.await;
        let release_result =
            repositories::release_runtime_library_graph_lock(graph_lock, library_id)
                .await
                .context("failed to release canonical graph advisory lock");
        match (result, release_result) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(release_error)) => Err(release_error),
            (Err(error), Err(release_error)) => Err(release_error).context(error.to_string()),
        }
    }

    pub async fn refresh_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> Result<u64> {
        GraphSummaryService::default().refresh_summaries(state, library_id, refresh).await
    }

    pub async fn invalidate_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> Result<u64> {
        GraphSummaryService::default().invalidate_summaries(state, library_id, refresh).await
    }

    pub async fn project_canonical_graph(
        &self,
        state: &AppState,
        scope: &GraphProjectionScope,
    ) -> Result<GraphProjectionOutcome> {
        crate::services::graph::projection::project_canonical_graph(state, scope).await
    }

    pub async fn rebuild_library_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<GraphProjectionOutcome> {
        crate::services::graph::rebuild::rebuild_library_graph(state, library_id).await
    }

    pub async fn reconcile_revision_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        activated_by_attempt_id: Option<Uuid>,
    ) -> Result<RevisionGraphReconcileOutcome> {
        let lock = self.library_merge_lock(library_id);
        let _guard = lock.lock().await;
        crate::services::graph::rebuild::reconcile_revision_graph(
            state,
            library_id,
            document_id,
            revision_id,
            activated_by_attempt_id,
        )
        .await
    }
}

#[derive(Debug, Clone)]
struct ArangoRevisionContext {
    revision_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_number: i64,
}

impl From<crate::infra::arangodb::document_store::KnowledgeRevisionRow> for ArangoRevisionContext {
    fn from(row: crate::infra::arangodb::document_store::KnowledgeRevisionRow) -> Self {
        Self {
            revision_id: row.revision_id,
            document_id: row.document_id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            revision_number: row.revision_number,
        }
    }
}
