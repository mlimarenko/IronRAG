use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        content::revision_text_state_is_readable,
        knowledge::{GraphEvidenceLiteralSpan, GraphEvidenceRecord, TypedTechnicalFact},
        runtime_graph::RuntimeNodeType,
    },
    infra::{
        arangodb::graph_store::{
            GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, KnowledgeEntityCandidateRow,
            KnowledgeEntityRow, KnowledgeEvidenceRow, KnowledgeRelationCandidateRow,
            KnowledgeRelationRow, NewKnowledgeEntity, NewKnowledgeEntityCandidate,
            NewKnowledgeEvidence, NewKnowledgeRelation, NewKnowledgeRelationCandidate,
            sanitize_graph_view_writes,
        },
        repositories,
    },
    services::{
        graph_extract::{
            GraphEntityCandidate, GraphExtractionCandidateSet, GraphRelationCandidate,
        },
        graph_identity,
        graph_merge::{self, GraphMergeOutcome, GraphMergeScope},
        graph_projection::{self, GraphProjectionOutcome, GraphProjectionScope},
        graph_rebuild::RevisionGraphReconcileOutcome,
        graph_summary::{GraphSummaryRefreshRequest, GraphSummaryService},
    },
};

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

#[derive(Debug, Clone, Default)]
struct ResolvedGraphEvidenceSupport {
    block_id: Option<Uuid>,
    fact_id: Option<Uuid>,
    literal_spans: Vec<GraphEvidenceLiteralSpan>,
    evidence_kind: String,
}

#[derive(Debug, Clone)]
struct ReconciledEntityCandidate {
    row: KnowledgeEntityCandidateRow,
    normalization_key: String,
}

#[derive(Debug, Clone)]
struct ReconciledRelationCandidate {
    row: KnowledgeRelationCandidateRow,
    subject_candidate_key: String,
    predicate: String,
    object_candidate_key: String,
    normalized_assertion: String,
}

#[derive(Debug, Default)]
struct MaterializedExtractCandidates {
    entity_candidates: Vec<NewKnowledgeEntityCandidate>,
    relation_candidates: Vec<NewKnowledgeRelationCandidate>,
}

#[derive(Clone, Default)]
pub struct GraphService;

impl GraphService {
    #[must_use]
    pub const fn new() -> Self {
        Self
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
        graph_quality_guard: &crate::services::graph_quality_guard::GraphQualityGuardService,
        scope: &GraphMergeScope,
        document: &repositories::DocumentRow,
        chunk: &repositories::ChunkRow,
        candidates: &crate::services::graph_extract::GraphExtractionCandidateSet,
        extraction_recovery: Option<&crate::domains::graph_quality::ExtractionRecoverySummary>,
    ) -> Result<GraphMergeOutcome> {
        graph_merge::merge_chunk_graph_candidates(
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
            .list_documents_by_library(library.workspace_id, library_id)
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

    async fn reconcile_arango_library_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let entity_candidates = state
            .arango_graph_store
            .list_entity_candidates_by_library(library_id)
            .await
            .context("failed to load arango entity candidates")?;
        let relation_candidates = state
            .arango_graph_store
            .list_relation_candidates_by_library(library_id)
            .await
            .context("failed to load arango relation candidates")?;
        self.reconcile_arango_candidates(
            state,
            library_id,
            entity_candidates,
            relation_candidates,
            alias_overrides,
        )
        .await
    }

    async fn reconcile_arango_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        entity_candidates: Vec<KnowledgeEntityCandidateRow>,
        relation_candidates: Vec<KnowledgeRelationCandidateRow>,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        #[derive(Debug)]
        struct EntityReconcileGroup {
            normalization_key: String,
            revision_context: ArangoRevisionContext,
            candidates: Vec<KnowledgeEntityCandidateRow>,
            entity_id: Uuid,
        }

        #[derive(Debug)]
        struct RelationReconcileGroup {
            revision_context: ArangoRevisionContext,
            candidates: Vec<ReconciledRelationCandidate>,
            relation_id: Uuid,
        }

        let entity_key_index = build_entity_candidate_key_index(&entity_candidates);
        let entity_candidates = entity_candidates
            .into_iter()
            .filter_map(|row| reconcile_entity_candidate_row(row, &entity_key_index))
            .collect::<Vec<_>>();
        let filtered_relation_candidates = relation_candidates
            .into_iter()
            .filter_map(|row| reconcile_relation_candidate_row(row, &entity_key_index))
            .filter(|candidate| {
                relation_candidate_keys_are_materializable(
                    &candidate.subject_candidate_key,
                    &candidate.predicate,
                    &candidate.object_candidate_key,
                )
            })
            .collect::<Vec<_>>();
        let entity_key_aliases = build_prefixed_entity_key_aliases(&entity_candidates);
        let entity_candidates = entity_candidates
            .into_iter()
            .map(|mut candidate| {
                if let Some(canonical_key) = entity_key_aliases.get(&candidate.normalization_key) {
                    candidate.normalization_key = canonical_key.clone();
                }
                candidate
            })
            .collect::<Vec<_>>();
        let filtered_relation_candidates = filtered_relation_candidates
            .into_iter()
            .map(|mut candidate| {
                apply_entity_key_aliases_to_relation_candidate(&mut candidate, &entity_key_aliases);
                candidate
            })
            .collect::<Vec<_>>();

        let mut revision_contexts = BTreeMap::<Uuid, ArangoRevisionContext>::new();
        for revision_id in entity_candidates
            .iter()
            .map(|candidate| candidate.row.revision_id)
            .chain(filtered_relation_candidates.iter().map(|candidate| candidate.row.revision_id))
            .collect::<BTreeSet<_>>()
        {
            if let Some(revision) = state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .context("failed to load revision for arango graph reconciliation")?
            {
                revision_contexts.insert(revision_id, ArangoRevisionContext::from(revision));
            }
        }

        let mut typed_facts_by_revision = BTreeMap::<Uuid, Vec<TypedTechnicalFact>>::new();
        for revision_id in revision_contexts.keys().copied() {
            let typed_facts = state
                .canonical_services
                .knowledge
                .list_typed_technical_facts(state, revision_id)
                .await
                .with_context(|| {
                    format!(
                        "failed to load typed technical facts for arango graph reconciliation revision {revision_id}"
                    )
                })?;
            typed_facts_by_revision.insert(revision_id, typed_facts);
        }

        let chunk_ids = entity_candidates
            .iter()
            .filter_map(|candidate| candidate.row.chunk_id)
            .chain(
                filtered_relation_candidates.iter().filter_map(|candidate| candidate.row.chunk_id),
            )
            .collect::<BTreeSet<_>>();
        let mut revision_chunk_ids = BTreeMap::<Uuid, BTreeSet<Uuid>>::new();
        for candidate in &entity_candidates {
            if let Some(chunk_id) = candidate.row.chunk_id {
                revision_chunk_ids.entry(candidate.row.revision_id).or_default().insert(chunk_id);
            }
        }
        for candidate in &filtered_relation_candidates {
            if let Some(chunk_id) = candidate.row.chunk_id {
                revision_chunk_ids.entry(candidate.row.revision_id).or_default().insert(chunk_id);
            }
        }
        let mut chunk_rows_by_id =
            BTreeMap::<Uuid, crate::infra::arangodb::document_store::KnowledgeChunkRow>::new();
        for chunk_id in chunk_ids {
            if let Some(chunk) =
                state.arango_document_store.get_chunk(chunk_id).await.with_context(|| {
                    format!("failed to load chunk {chunk_id} for arango graph reconciliation")
                })?
            {
                chunk_rows_by_id.insert(chunk_id, chunk);
            }
        }

        let mut outcome = ArangoGraphRebuildOutcome {
            scanned_entity_candidates: entity_candidates.len(),
            scanned_relation_candidates: filtered_relation_candidates.len(),
            ..Default::default()
        };

        for (revision_id, revision_context) in &revision_contexts {
            self.upsert_revision_edges(state, revision_context).await?;
            outcome.upserted_document_revision_edges += 1;
            if let Some(chunk_ids) = revision_chunk_ids.get(revision_id) {
                for chunk_id in chunk_ids {
                    self.upsert_chunk_edge(state, revision_context, *chunk_id).await?;
                    outcome.upserted_revision_chunk_edges += 1;
                }
            }
        }

        let mut entity_groups = BTreeMap::<String, Vec<ReconciledEntityCandidate>>::new();
        for candidate in entity_candidates {
            entity_groups.entry(candidate.normalization_key.clone()).or_default().push(candidate);
        }

        let mut entity_reconcile_groups = Vec::<EntityReconcileGroup>::new();
        let mut entity_requests = Vec::<NewKnowledgeEntity>::new();
        let mut entity_request_ids = BTreeSet::<Uuid>::new();
        for (normalization_key, rows) in entity_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("entity candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let canonical_label = select_canonical_entity_label(&rows, &normalization_key)
                .unwrap_or_else(|| canonical_label_from_node_key(&normalization_key));
            let entity_type = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.row.candidate_type.trim().is_empty())
                        .then(|| candidate.row.candidate_type.trim().to_string())
                })
                .unwrap_or_else(|| {
                    graph_identity::runtime_node_type_slug(
                        &graph_identity::runtime_node_type_from_key(&normalization_key),
                    )
                    .to_string()
                });
            let alias_rows = rows.iter().map(|candidate| candidate.row.clone()).collect::<Vec<_>>();
            let aliases = self.collect_entity_aliases(
                &alias_rows,
                alias_overrides,
                &normalization_key,
                &canonical_label,
            );
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let entity_id = canonical_entity_id(library_id, &normalization_key);
            entity_request_ids.insert(entity_id);
            entity_requests.push(NewKnowledgeEntity {
                entity_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                canonical_label,
                aliases: aliases.into_iter().collect(),
                entity_type,
                summary: None,
                confidence,
                support_count: rows.len() as i64,
                freshness_generation: revision_context.revision_number,
                entity_state: "active".to_string(),
                created_at: None,
                updated_at: Some(Utc::now()),
            });
            entity_reconcile_groups.push(EntityReconcileGroup {
                normalization_key,
                revision_context: revision_context.clone(),
                candidates: rows.into_iter().map(|candidate| candidate.row).collect(),
                entity_id,
            });
        }

        let mut relation_groups = BTreeMap::<String, Vec<ReconciledRelationCandidate>>::new();
        for candidate in filtered_relation_candidates {
            relation_groups
                .entry(candidate.normalized_assertion.clone())
                .or_default()
                .push(candidate);
        }

        let mut relation_reconcile_groups = Vec::<RelationReconcileGroup>::new();
        let mut relation_requests = Vec::<NewKnowledgeRelation>::new();
        let mut placeholder_entity_requests = BTreeMap::<Uuid, NewKnowledgeEntity>::new();
        for (normalized_assertion, rows) in relation_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("relation candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let predicate = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.predicate.trim().is_empty())
                        .then(|| candidate.predicate.trim().to_string())
                })
                .unwrap_or_else(|| "related_to".to_string());
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let relation_id = canonical_relation_id(library_id, &normalized_assertion);
            relation_requests.push(NewKnowledgeRelation {
                relation_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                predicate,
                normalized_assertion,
                confidence,
                support_count: rows.len() as i64,
                contradiction_state: "unknown".to_string(),
                freshness_generation: revision_context.revision_number,
                relation_state: "active".to_string(),
                created_at: None,
                updated_at: Some(Utc::now()),
            });
            for candidate in &rows {
                for normalization_key in
                    [&candidate.subject_candidate_key, &candidate.object_candidate_key]
                {
                    let Some((node_type, canonical_label)) =
                        placeholder_entity_parts_from_key(normalization_key)
                    else {
                        continue;
                    };
                    let entity_id = canonical_entity_id(library_id, normalization_key);
                    if entity_request_ids.contains(&entity_id) {
                        continue;
                    }
                    let entry = placeholder_entity_requests.entry(entity_id).or_insert_with(|| {
                        NewKnowledgeEntity {
                            entity_id,
                            workspace_id: revision_context.workspace_id,
                            library_id,
                            canonical_label: canonical_label.clone(),
                            aliases: vec![canonical_label.clone()],
                            entity_type: graph_identity::runtime_node_type_slug(&node_type)
                                .to_string(),
                            summary: None,
                            confidence: None,
                            support_count: 0,
                            freshness_generation: revision_context.revision_number,
                            entity_state: "active".to_string(),
                            created_at: None,
                            updated_at: Some(Utc::now()),
                        }
                    });
                    entry.support_count += 1;
                    entry.freshness_generation =
                        entry.freshness_generation.max(revision_context.revision_number);
                    entry.updated_at = Some(Utc::now());
                }
            }
            relation_reconcile_groups.push(RelationReconcileGroup {
                revision_context: revision_context.clone(),
                candidates: rows,
                relation_id,
            });
        }

        entity_requests.extend(placeholder_entity_requests.into_values());
        self.reset_arango_library_materialization(state, library_id).await?;
        let entity_rows = state.arango_graph_store.upsert_entities(&entity_requests).await?;
        let entity_by_id =
            entity_rows.into_iter().map(|row| (row.entity_id, row)).collect::<BTreeMap<_, _>>();

        for group in entity_reconcile_groups {
            let entity = entity_by_id.get(&group.entity_id).ok_or_else(|| {
                anyhow::anyhow!("missing canonical entity {} after bulk upsert", group.entity_id)
            })?;
            outcome.upserted_entities += 1;
            for candidate in group.candidates {
                let supporting_chunk =
                    candidate.chunk_id.and_then(|chunk_id| chunk_rows_by_id.get(&chunk_id));
                let revision_facts = typed_facts_by_revision
                    .get(&candidate.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_entity_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    entity,
                    &group.normalization_key,
                    supporting_chunk,
                    revision_facts,
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_entity_edges += 1;
                if candidate.chunk_id.is_some() {
                    outcome.upserted_revision_chunk_edges += 0;
                    outcome.upserted_chunk_entity_edges += 1;
                }
            }
        }

        let relation_rows = state.arango_graph_store.upsert_relations(&relation_requests).await?;
        let relation_by_id =
            relation_rows.into_iter().map(|row| (row.relation_id, row)).collect::<BTreeMap<_, _>>();

        for group in relation_reconcile_groups {
            let relation = relation_by_id.get(&group.relation_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "missing canonical relation {} after bulk upsert",
                    group.relation_id
                )
            })?;
            outcome.upserted_relations += 1;
            for candidate in group.candidates {
                let subject_id = canonical_entity_id(library_id, &candidate.subject_candidate_key);
                let object_id = canonical_entity_id(library_id, &candidate.object_candidate_key);
                let subject = entity_by_id.get(&subject_id).ok_or_else(|| {
                    anyhow::anyhow!("missing subject placeholder entity {}", subject_id)
                })?;
                let object = entity_by_id.get(&object_id).ok_or_else(|| {
                    anyhow::anyhow!("missing object placeholder entity {}", object_id)
                })?;
                self.upsert_relation_edges(state, relation, subject, object).await?;
                let supporting_chunk =
                    candidate.row.chunk_id.and_then(|chunk_id| chunk_rows_by_id.get(&chunk_id));
                let revision_facts = typed_facts_by_revision
                    .get(&candidate.row.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_relation_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    relation,
                    supporting_chunk,
                    revision_facts,
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_relation_subject_edges += 1;
                outcome.upserted_relation_object_edges += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_relation_edges += 1;
            }
        }

        Ok(outcome)
    }

    async fn materialize_current_candidate_batch(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        candidates: &GraphExtractionCandidateSet,
        mark_existing_only: bool,
    ) -> Result<()> {
        let revision_context = ArangoRevisionContext::from(revision.clone());
        let entity_key_index = build_relation_entity_key_index(candidates);
        let entity_alias_overrides = self.build_alias_overrides(candidates, &entity_key_index);
        let chunk_row =
            state.arango_document_store.get_chunk(chunk_id).await.with_context(|| {
                format!("failed to load chunk {chunk_id} for graph materialization")
            })?;
        let revision_facts = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(state, revision.revision_id)
            .await
            .with_context(|| {
                format!(
                    "failed to load typed technical facts for graph materialization revision {}",
                    revision.revision_id
                )
            })?;
        self.upsert_revision_edges(state, &revision_context).await?;
        self.upsert_chunk_edge(state, &revision_context, chunk_id).await?;

        for entity in &candidates.entities {
            let candidate =
                self.build_entity_candidate_row(revision, chunk_id, entity, &entity_key_index);
            let candidate_row = state
                .arango_graph_store
                .upsert_entity_candidate(&candidate)
                .await
                .context("failed to upsert arango entity candidate")?;
            if !mark_existing_only {
                let entity_row = self
                    .upsert_canonical_entity(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.normalization_key,
                        candidate.candidate_label.trim(),
                        &candidate.candidate_type,
                        entity_alias_overrides
                            .get(&candidate.normalization_key)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                        candidate.confidence,
                        1,
                        revision.revision_number,
                    )
                    .await?;
                self.upsert_current_entity_evidence(
                    state,
                    &revision_context,
                    &candidate_row,
                    &entity_row,
                    &candidate_row.normalization_key,
                    chunk_row.as_ref(),
                    revision_facts.as_slice(),
                )
                .await?;
                self.upsert_chunk_mentions_entity_edge(
                    state,
                    chunk_id,
                    entity_row.entity_id,
                    candidate_row.confidence,
                )
                .await?;
            }
        }

        for relation in &candidates.relations {
            if relation_fields_are_semantically_empty(
                &relation.source_label,
                &relation.relation_type,
                &relation.target_label,
            ) {
                continue;
            }
            let candidate =
                self.build_relation_candidate_row(revision, chunk_id, relation, &entity_key_index);
            let candidate_row = state
                .arango_graph_store
                .upsert_relation_candidate(&candidate)
                .await
                .context("failed to upsert arango relation candidate")?;
            if !mark_existing_only {
                let relation_row = self
                    .upsert_canonical_relation(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.normalized_assertion,
                        candidate.predicate.trim(),
                        candidate.confidence,
                        1,
                        revision.revision_number,
                    )
                    .await?;
                let subject = self
                    .upsert_placeholder_entity_for_key(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.subject_candidate_key,
                    )
                    .await?;
                let object = self
                    .upsert_placeholder_entity_for_key(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.object_candidate_key,
                    )
                    .await?;
                self.upsert_relation_edges(state, &relation_row, &subject, &object).await?;
                self.upsert_current_relation_evidence(
                    state,
                    &revision_context,
                    &candidate_row,
                    &relation_row,
                    chunk_row.as_ref(),
                    revision_facts.as_slice(),
                )
                .await?;
            }
        }

        Ok(())
    }

    fn build_alias_overrides(
        &self,
        candidates: &GraphExtractionCandidateSet,
        entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut overrides = BTreeMap::<String, BTreeSet<String>>::new();
        for entity in &candidates.entities {
            let key = entity_key_index.canonical_node_key_for_label(&entity.label);
            let aliases = overrides.entry(key).or_default();
            aliases.insert(entity.label.trim().to_string());
            for alias in &entity.aliases {
                let trimmed = alias.trim();
                if !trimmed.is_empty() {
                    aliases.insert(trimmed.to_string());
                }
            }
        }
        overrides
    }

    fn build_entity_candidate_row(
        &self,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        entity: &GraphEntityCandidate,
        entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeEntityCandidate {
        let normalization_key = entity_key_index.canonical_node_key_for_label(&entity.label);
        let canonical_node_type = graph_identity::runtime_node_type_from_key(&normalization_key);
        let candidate_id = canonical_entity_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalization_key,
            &entity.label,
            &canonical_node_type,
        );
        NewKnowledgeEntityCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            candidate_label: entity.label.trim().to_string(),
            candidate_type: graph_identity::runtime_node_type_slug(&canonical_node_type)
                .to_string(),
            normalization_key,
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }

    fn build_relation_candidate_row(
        &self,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        relation: &GraphRelationCandidate,
        entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeRelationCandidate {
        let subject_candidate_key =
            entity_key_index.canonical_node_key_for_label(&relation.source_label);
        let object_candidate_key =
            entity_key_index.canonical_node_key_for_label(&relation.target_label);
        let normalized_assertion = canonical_relation_assertion_from_keys(
            &subject_candidate_key,
            &relation.relation_type,
            &object_candidate_key,
        );
        let candidate_id = canonical_relation_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalized_assertion,
            &relation.source_label,
            &relation.target_label,
            &relation.relation_type,
        );
        NewKnowledgeRelationCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            subject_label: relation.source_label.trim().to_string(),
            subject_candidate_key,
            predicate: relation.relation_type.trim().to_string(),
            object_label: relation.target_label.trim().to_string(),
            object_candidate_key,
            normalized_assertion,
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }

    async fn upsert_canonical_entity(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalization_key: &str,
        canonical_label: &str,
        entity_type: &str,
        aliases: BTreeSet<String>,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeEntityRow> {
        let entity_id = canonical_entity_id(library_id, normalization_key);
        let existing = state
            .arango_graph_store
            .get_entity_by_id(entity_id)
            .await
            .context("failed to load canonical entity before upsert")?;
        let mut merged_aliases =
            existing.as_ref().map(|row| row.aliases.clone()).unwrap_or_default();
        for alias in aliases {
            if !merged_aliases.iter().any(|existing| existing == &alias) {
                merged_aliases.push(alias);
            }
        }
        if !merged_aliases.iter().any(|alias| alias == canonical_label) {
            merged_aliases.push(canonical_label.to_string());
        }
        let summary = existing.as_ref().and_then(|row| row.summary.clone());
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let entity = NewKnowledgeEntity {
            entity_id,
            workspace_id,
            library_id,
            canonical_label: canonical_label.to_string(),
            aliases: merged_aliases,
            entity_type: entity_type.to_string(),
            summary,
            confidence,
            support_count,
            freshness_generation,
            entity_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut last_err = None;
        for attempt in 0..3 {
            match state.arango_graph_store.upsert_entity(&entity).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e).context("failed to upsert canonical arango entity");
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("canonical arango entity upsert exhausted retries without an error")
        })?)
        .context("failed to upsert canonical arango entity after retries")
    }

    async fn upsert_placeholder_entity_for_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        canonical_key: &str,
    ) -> Result<KnowledgeEntityRow> {
        let normalization_key = canonical_key.trim();
        let (node_type, canonical_label) = placeholder_entity_parts_from_key(normalization_key)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid canonical entity key `{normalization_key}` while materializing relation endpoints"
                )
            })?;
        let aliases = {
            let mut set = BTreeSet::new();
            set.insert(canonical_label.clone());
            set
        };
        self.upsert_canonical_entity(
            state,
            library_id,
            workspace_id,
            normalization_key,
            &canonical_label,
            graph_identity::runtime_node_type_slug(&node_type),
            aliases,
            None,
            1,
            0,
        )
        .await
    }

    async fn upsert_canonical_relation(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalized_assertion: &str,
        predicate: &str,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeRelationRow> {
        let relation_id = canonical_relation_id(library_id, normalized_assertion);
        let existing = state
            .arango_graph_store
            .get_relation_by_id(relation_id)
            .await
            .context("failed to load canonical relation before upsert")?;
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let relation = NewKnowledgeRelation {
            relation_id,
            workspace_id,
            library_id,
            predicate: predicate.to_string(),
            normalized_assertion: normalized_assertion.to_string(),
            confidence,
            support_count,
            contradiction_state: existing
                .as_ref()
                .map(|row| row.contradiction_state.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            freshness_generation,
            relation_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut last_err = None;
        for attempt in 0..3 {
            match state.arango_graph_store.upsert_relation(&relation).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e).context("failed to upsert canonical arango relation");
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("canonical arango relation upsert exhausted retries without an error")
        })?)
        .context("failed to upsert canonical arango relation after retries")
    }

    async fn upsert_current_entity_evidence<C>(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        candidate: &C,
        entity: &KnowledgeEntityRow,
        canonical_key: &str,
        supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
        revision_facts: &[TypedTechnicalFact],
    ) -> Result<KnowledgeEvidenceRow>
    where
        C: ArangoEntityEvidenceCandidate,
    {
        let evidence_id = canonical_evidence_id(
            revision.library_id,
            revision.revision_id,
            candidate.chunk_id(),
            "entity",
            canonical_key,
        );
        let excerpt = candidate.candidate_label().to_string();
        let support = resolve_entity_evidence_support(
            candidate.candidate_label(),
            excerpt.as_str(),
            supporting_chunk,
            revision_facts,
        );
        let evidence = GraphEvidenceRecord {
            evidence_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: candidate.chunk_id(),
            block_id: support.block_id,
            fact_id: support.fact_id,
            quote_text: excerpt,
            literal_spans: support.literal_spans,
            confidence: candidate.confidence(),
            evidence_kind: support.evidence_kind,
            created_at: Utc::now(),
        };
        let row = state
            .arango_graph_store
            .upsert_evidence_with_edges(
                &graph_evidence_record_to_new_evidence(
                    revision.workspace_id,
                    revision.document_id,
                    revision.revision_number,
                    candidate.extraction_method(),
                    &evidence,
                ),
                Some(revision.revision_id),
                Some(entity.entity_id),
                None,
                evidence.fact_id,
            )
            .await
            .context("failed to upsert arango entity evidence")?;
        if let Some(chunk_id) = candidate.chunk_id() {
            self.upsert_chunk_mentions_entity_edge(
                state,
                chunk_id,
                entity.entity_id,
                candidate.confidence(),
            )
            .await?;
        }
        Ok(row)
    }

    async fn upsert_current_relation_evidence<C>(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        candidate: &C,
        relation: &KnowledgeRelationRow,
        supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
        revision_facts: &[TypedTechnicalFact],
    ) -> Result<KnowledgeEvidenceRow>
    where
        C: ArangoRelationEvidenceCandidate,
    {
        let evidence_id = canonical_evidence_id(
            revision.library_id,
            revision.revision_id,
            candidate.chunk_id(),
            "relation",
            candidate.normalized_assertion(),
        );
        let excerpt = format!(
            "{} {} {}",
            candidate.subject_candidate_key(),
            candidate.predicate(),
            candidate.object_candidate_key()
        );
        let support = resolve_relation_evidence_support(
            candidate.subject_candidate_key(),
            candidate.predicate(),
            candidate.object_candidate_key(),
            excerpt.as_str(),
            supporting_chunk,
            revision_facts,
        );
        let evidence = GraphEvidenceRecord {
            evidence_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: candidate.chunk_id(),
            block_id: support.block_id,
            fact_id: support.fact_id,
            quote_text: excerpt,
            literal_spans: support.literal_spans,
            confidence: candidate.confidence(),
            evidence_kind: support.evidence_kind,
            created_at: Utc::now(),
        };
        let row = state
            .arango_graph_store
            .upsert_evidence_with_edges(
                &graph_evidence_record_to_new_evidence(
                    revision.workspace_id,
                    revision.document_id,
                    revision.revision_number,
                    candidate.extraction_method(),
                    &evidence,
                ),
                Some(revision.revision_id),
                None,
                Some(relation.relation_id),
                evidence.fact_id,
            )
            .await
            .context("failed to upsert arango relation evidence")?;
        let subject = self
            .upsert_placeholder_entity_for_key(
                state,
                revision.library_id,
                revision.workspace_id,
                candidate.subject_candidate_key(),
            )
            .await?;
        let object = self
            .upsert_placeholder_entity_for_key(
                state,
                revision.library_id,
                revision.workspace_id,
                candidate.object_candidate_key(),
            )
            .await?;
        self.upsert_relation_edges(state, relation, &subject, &object).await?;
        Ok(row)
    }

    async fn upsert_relation_edges(
        &self,
        state: &AppState,
        relation: &KnowledgeRelationRow,
        subject: &KnowledgeEntityRow,
        object: &KnowledgeEntityRow,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_relation_subject_edge",
            canonical_edge_relation_key(relation.relation_id, subject.entity_id, "subject"),
            "knowledge_relation",
            relation.relation_id,
            "knowledge_entity",
            subject.entity_id,
            json!({}),
        )
        .await?;
        self.upsert_arango_edge(
            state,
            "knowledge_relation_object_edge",
            canonical_edge_relation_key(relation.relation_id, object.entity_id, "object"),
            "knowledge_relation",
            relation.relation_id,
            "knowledge_entity",
            object.entity_id,
            json!({}),
        )
        .await
    }

    async fn upsert_revision_edges(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_document_revision_edge",
            canonical_document_revision_edge_key(revision.document_id, revision.revision_id),
            "knowledge_document",
            revision.document_id,
            "knowledge_revision",
            revision.revision_id,
            json!({}),
        )
        .await
    }

    async fn upsert_chunk_edge(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        chunk_id: Uuid,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_revision_chunk_edge",
            canonical_revision_chunk_edge_key(revision.revision_id, chunk_id),
            "knowledge_revision",
            revision.revision_id,
            "knowledge_chunk",
            chunk_id,
            json!({}),
        )
        .await
    }

    async fn upsert_chunk_mentions_entity_edge(
        &self,
        state: &AppState,
        chunk_id: Uuid,
        entity_id: Uuid,
        score: Option<f64>,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_chunk_mentions_entity_edge",
            canonical_chunk_mentions_entity_edge_key(chunk_id, entity_id),
            "knowledge_chunk",
            chunk_id,
            "knowledge_entity",
            entity_id,
            json!({
                "rank": 1,
                "score": score,
                "inclusionReason": "graph_extract_entity_candidate",
            }),
        )
        .await
    }

    async fn upsert_arango_edge(
        &self,
        state: &AppState,
        collection: &str,
        key: String,
        from_collection: &str,
        from_id: Uuid,
        to_collection: &str,
        to_id: Uuid,
        extra_fields: serde_json::Value,
    ) -> Result<()> {
        let client = state.arango_graph_store.client();
        let query = "UPSERT { _key: @key }
                     INSERT {
                        _key: @key,
                        _from: @from,
                        _to: @to,
                        created_at: @created_at,
                        updated_at: @updated_at,
                        payload: @payload
                     }
                     UPDATE {
                        _from: @from,
                        _to: @to,
                        updated_at: @updated_at,
                        payload: @payload
                     }
                     IN @@collection
                     RETURN NEW";
        let bind_vars = json!({
            "@collection": collection,
            "key": key,
            "from": format!("{from_collection}/{from_id}"),
            "to": format!("{to_collection}/{to_id}"),
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
            "payload": extra_fields,
        });
        let mut last_err = None;
        for attempt in 0..3u32 {
            match client.query_json(query, bind_vars.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e)
                        .with_context(|| format!("failed to upsert arango edge in {collection}"));
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("arango edge upsert exhausted retries without an error")
        })?)
        .with_context(|| format!("failed to upsert arango edge in {collection} after retries"))
    }

    async fn build_and_refresh_arango_graph_from_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.reconcile_arango_library_candidates(state, library_id, alias_overrides).await
    }

    async fn refresh_arango_library_candidate_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let library = state
            .canonical_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for arango candidate rebuild")?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id)
            .await
            .context("failed to list documents for arango candidate rebuild")?;
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
                state.arango_document_store.get_revision(revision_id).await.with_context(|| {
                    format!("failed to load arango revision {revision_id} for candidate rebuild")
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
            .arango_graph_store
            .delete_entity_candidates_by_library(library_id)
            .await
            .context("failed to clear stale arango entity candidates before rebuild")?;
        state
            .arango_graph_store
            .delete_relation_candidates_by_library(library_id)
            .await
            .context("failed to clear stale arango relation candidates before rebuild")?;

        for batch in entity_candidates.chunks(256) {
            state.arango_graph_store.upsert_entity_candidates(batch).await.with_context(|| {
                format!("failed to persist {} arango entity candidates during rebuild", batch.len())
            })?;
        }

        for batch in relation_candidates.chunks(256) {
            state.arango_graph_store.upsert_relation_candidates(batch).await.with_context(
                || {
                    format!(
                        "failed to persist {} arango relation candidates during rebuild",
                        batch.len()
                    )
                },
            )?;
        }

        Ok(())
    }

    async fn load_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
    ) -> Result<MaterializedExtractCandidates> {
        let chunk_results =
            repositories::extract_repository::list_ready_extract_chunk_results_by_revision(
                &state.persistence.postgres,
                revision.revision_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load canonical extract chunk results for revision {}",
                    revision.revision_id
                )
            })?;

        self.collect_revision_materialized_extract_candidates(state, revision, &chunk_results).await
    }

    async fn collect_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
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
                        "failed to load canonical extract node candidates for chunk result {}",
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
                        "failed to load canonical extract edge candidates for chunk result {}",
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

    async fn reset_arango_library_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        state
            .arango_graph_store
            .reset_library_materialized_graph(library_id)
            .await
            .context("failed to reset arango graph materialization")?;
        state
            .arango_search_store
            .delete_entity_vectors_by_library(library_id)
            .await
            .context("failed to delete stale entity vectors for graph reset")?;
        Ok(())
    }

    async fn recalculate_arango_library_generations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let library = state
            .canonical_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for generation refresh")?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id)
            .await
            .context("failed to list documents for generation refresh")?;
        let mut active_text_generation = 0i64;
        let mut active_vector_generation = 0i64;
        let mut active_graph_generation = 0i64;
        let mut has_ready_text = false;
        let mut has_ready_vector = false;
        let mut has_ready_graph = false;

        for document in documents {
            let revisions = state
                .arango_document_store
                .list_revisions_by_document(document.document_id)
                .await
                .context("failed to list revisions for generation refresh")?;
            for revision in revisions {
                if revision_text_state_is_readable(&revision.text_state) {
                    has_ready_text = true;
                    active_text_generation = active_text_generation.max(revision.revision_number);
                }
                if revision.vector_state == "ready" {
                    has_ready_vector = true;
                    active_vector_generation =
                        active_vector_generation.max(revision.revision_number);
                }
                if revision.graph_state == "ready" {
                    has_ready_graph = true;
                    active_graph_generation = active_graph_generation.max(revision.revision_number);
                }
            }
        }

        let _ = state
            .canonical_services
            .knowledge
            .refresh_library_generation(
                state,
                crate::services::knowledge_service::RefreshKnowledgeLibraryGenerationCommand {
                    generation_id: Uuid::now_v7(),
                    workspace_id: library.workspace_id,
                    library_id,
                    active_text_generation: if has_ready_text { active_text_generation } else { 0 },
                    active_vector_generation: if has_ready_vector {
                        active_vector_generation
                    } else {
                        0
                    },
                    active_graph_generation: if has_ready_graph {
                        active_graph_generation
                    } else {
                        0
                    },
                    degraded_state: if has_ready_text && has_ready_vector && has_ready_graph {
                        "ready".to_string()
                    } else {
                        "degraded".to_string()
                    },
                },
            )
            .await
            .context("failed to refresh arango library generation")?;
        Ok(())
    }

    fn collect_entity_aliases(
        &self,
        rows: &[KnowledgeEntityCandidateRow],
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
        normalization_key: &str,
        canonical_label: &str,
    ) -> BTreeSet<String> {
        let mut aliases = BTreeSet::<String>::new();
        if !canonical_label.trim().is_empty() {
            aliases.insert(canonical_label.trim().to_string());
        }
        for row in rows {
            if !row.candidate_label.trim().is_empty() {
                aliases.insert(row.candidate_label.trim().to_string());
            }
        }
        if let Some(overrides) = alias_overrides {
            if let Some(values) = overrides.get(normalization_key) {
                aliases.extend(values.iter().cloned());
            }
        }
        aliases
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
        graph_projection::project_canonical_graph(state, scope).await
    }

    pub async fn rebuild_library_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<GraphProjectionOutcome> {
        crate::services::graph_rebuild::rebuild_library_graph(state, library_id).await
    }

    pub async fn reconcile_revision_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        activated_by_attempt_id: Option<Uuid>,
    ) -> Result<RevisionGraphReconcileOutcome> {
        crate::services::graph_rebuild::reconcile_revision_graph(
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

trait ArangoEntityEvidenceCandidate {
    fn chunk_id(&self) -> Option<Uuid>;
    fn candidate_label(&self) -> &str;
    fn extraction_method(&self) -> &str;
    fn confidence(&self) -> Option<f64>;
}

impl ArangoEntityEvidenceCandidate for KnowledgeEntityCandidateRow {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn candidate_label(&self) -> &str {
        &self.candidate_label
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoEntityEvidenceCandidate for NewKnowledgeEntityCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn candidate_label(&self) -> &str {
        &self.candidate_label
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

trait ArangoRelationEvidenceCandidate {
    fn chunk_id(&self) -> Option<Uuid>;
    fn subject_candidate_key(&self) -> &str;
    fn predicate(&self) -> &str;
    fn object_candidate_key(&self) -> &str;
    fn normalized_assertion(&self) -> &str;
    fn extraction_method(&self) -> &str;
    fn confidence(&self) -> Option<f64>;
}

impl ArangoRelationEvidenceCandidate for KnowledgeRelationCandidateRow {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoRelationEvidenceCandidate for NewKnowledgeRelationCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoRelationEvidenceCandidate for ReconciledRelationCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.row.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.row.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.row.confidence
    }
}

fn graph_evidence_record_to_new_evidence(
    workspace_id: Uuid,
    document_id: Uuid,
    freshness_generation: i64,
    extraction_method: &str,
    record: &GraphEvidenceRecord,
) -> NewKnowledgeEvidence {
    let span_start = record.literal_spans.iter().map(|span| span.start_offset).min();
    let span_end = record.literal_spans.iter().map(|span| span.end_offset).max();
    NewKnowledgeEvidence {
        evidence_id: record.evidence_id,
        workspace_id,
        library_id: record.library_id,
        document_id,
        revision_id: record.revision_id,
        chunk_id: record.chunk_id,
        block_id: record.block_id,
        fact_id: record.fact_id,
        span_start,
        span_end,
        quote_text: record.quote_text.clone(),
        literal_spans_json: serde_json::to_value(&record.literal_spans)
            .unwrap_or_else(|_| json!([])),
        evidence_kind: record.evidence_kind.clone(),
        extraction_method: extraction_method.to_string(),
        confidence: record.confidence,
        evidence_state: "active".to_string(),
        freshness_generation,
        created_at: Some(record.created_at),
        updated_at: Some(Utc::now()),
    }
}

fn resolve_entity_evidence_support(
    candidate_label: &str,
    quote_text: &str,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
    revision_facts: &[TypedTechnicalFact],
) -> ResolvedGraphEvidenceSupport {
    let fact = revision_facts
        .iter()
        .filter(|fact| fact_supports_chunk(fact, supporting_chunk))
        .find(|fact| technical_fact_matches_literals(fact, &[candidate_label]));
    let block_id = fact
        .and_then(|fact| fact.support_block_ids.first().copied())
        .or_else(|| supporting_chunk.and_then(|chunk| chunk.support_block_ids.first().copied()));
    ResolvedGraphEvidenceSupport {
        block_id,
        fact_id: fact.map(|fact| fact.fact_id),
        literal_spans: literal_spans_for_quote(quote_text, &[candidate_label]),
        evidence_kind: if fact.is_some() {
            "entity_fact_support".to_string()
        } else if block_id.is_some() {
            "entity_block_support".to_string()
        } else {
            "entity_candidate".to_string()
        },
    }
}

fn resolve_relation_evidence_support(
    subject: &str,
    predicate: &str,
    object: &str,
    quote_text: &str,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
    revision_facts: &[TypedTechnicalFact],
) -> ResolvedGraphEvidenceSupport {
    let fact = revision_facts
        .iter()
        .filter(|fact| fact_supports_chunk(fact, supporting_chunk))
        .find(|fact| technical_fact_matches_relation(fact, subject, predicate, object));
    let block_id = fact
        .and_then(|fact| fact.support_block_ids.first().copied())
        .or_else(|| supporting_chunk.and_then(|chunk| chunk.support_block_ids.first().copied()));
    ResolvedGraphEvidenceSupport {
        block_id,
        fact_id: fact.map(|fact| fact.fact_id),
        literal_spans: literal_spans_for_quote(quote_text, &[subject, predicate, object]),
        evidence_kind: if fact.is_some() {
            "relation_fact_support".to_string()
        } else if block_id.is_some() {
            "relation_block_support".to_string()
        } else {
            "relation_candidate".to_string()
        },
    }
}

fn fact_supports_chunk(
    fact: &TypedTechnicalFact,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
) -> bool {
    let Some(chunk) = supporting_chunk else {
        return true;
    };
    fact.support_chunk_ids.contains(&chunk.chunk_id)
        || fact.support_block_ids.iter().any(|block_id| chunk.support_block_ids.contains(block_id))
}

fn technical_fact_matches_literals(fact: &TypedTechnicalFact, literals: &[&str]) -> bool {
    let haystack = technical_fact_match_haystack(fact);
    literals
        .iter()
        .map(|literal| normalize_evidence_literal(literal))
        .filter(|literal| !literal.is_empty())
        .all(|literal| haystack.contains(&literal))
}

fn technical_fact_matches_relation(
    fact: &TypedTechnicalFact,
    subject: &str,
    predicate: &str,
    object: &str,
) -> bool {
    let haystack = technical_fact_match_haystack(fact);
    let needles = [subject, predicate, object]
        .into_iter()
        .map(normalize_evidence_literal)
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    if needles.is_empty() {
        return false;
    }
    let matched = needles.iter().filter(|literal| haystack.contains(literal.as_str())).count();
    matched >= needles.len().min(2)
}

fn technical_fact_match_haystack(fact: &TypedTechnicalFact) -> String {
    let mut parts = Vec::<String>::new();
    parts.push(normalize_evidence_literal(&fact.display_value));
    parts.push(normalize_evidence_literal(&fact.canonical_value.canonical_string()));
    for qualifier in &fact.qualifiers {
        parts.push(normalize_evidence_literal(&qualifier.key));
        parts.push(normalize_evidence_literal(&qualifier.value));
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" ")
}

fn literal_spans_for_quote(quote_text: &str, literals: &[&str]) -> Vec<GraphEvidenceLiteralSpan> {
    let mut spans = Vec::<GraphEvidenceLiteralSpan>::new();
    for literal in literals {
        let trimmed = literal.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(start_offset) = quote_text.find(trimmed) {
            let end_offset = start_offset.saturating_add(trimmed.len());
            spans.push(GraphEvidenceLiteralSpan {
                start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                literal: trimmed.to_string(),
            });
        }
    }
    spans
}

pub(crate) fn relation_fields_are_semantically_empty(
    subject: &str,
    predicate: &str,
    object: &str,
) -> bool {
    [
        normalize_evidence_literal(subject),
        normalize_evidence_literal(predicate),
        normalize_evidence_literal(object),
    ]
    .into_iter()
    .any(|value| value.is_empty())
}

fn normalize_evidence_literal(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '"' | '\'' | '`'))
        .flat_map(char::to_lowercase)
        .collect()
}

#[must_use]
fn placeholder_entity_parts_from_key(canonical_key: &str) -> Option<(RuntimeNodeType, String)> {
    let normalization_key = canonical_key.trim();
    if normalization_key.is_empty() {
        return None;
    }
    let canonical_label = normalization_key
        .split_once(':')
        .map(|(_, label)| label)
        .unwrap_or(normalization_key)
        .trim();
    if canonical_label.is_empty() {
        return None;
    }
    Some((
        graph_identity::runtime_node_type_from_key(normalization_key),
        canonical_label.to_string(),
    ))
}

#[must_use]
fn build_prefixed_entity_key_aliases(
    entity_candidates: &[ReconciledEntityCandidate],
) -> BTreeMap<String, String> {
    let known_keys = entity_candidates
        .iter()
        .map(|candidate| candidate.normalization_key.clone())
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeMap::<String, String>::new();

    for key in &known_keys {
        let Some((node_type, identity)) = key.split_once(':') else {
            continue;
        };
        let mut parts = identity.split('_');
        parts.next();
        let stripped_identity = parts.collect::<Vec<_>>().join("_");
        if stripped_identity.is_empty() {
            continue;
        }
        let stripped_key = format!("{node_type}:{stripped_identity}");
        if known_keys.contains(&stripped_key) {
            aliases.insert(stripped_key, key.clone());
        }
    }

    aliases
}

fn apply_entity_key_aliases_to_relation_candidate(
    candidate: &mut ReconciledRelationCandidate,
    aliases: &BTreeMap<String, String>,
) {
    if let Some(canonical_key) = aliases.get(&candidate.subject_candidate_key) {
        candidate.subject_candidate_key = canonical_key.clone();
    }
    if let Some(canonical_key) = aliases.get(&candidate.object_candidate_key) {
        candidate.object_candidate_key = canonical_key.clone();
    }
    candidate.normalized_assertion = graph_identity::canonical_edge_key(
        &candidate.subject_candidate_key,
        &candidate.predicate,
        &candidate.object_candidate_key,
    );
}

#[must_use]
fn select_canonical_entity_label(
    rows: &[ReconciledEntityCandidate],
    normalization_key: &str,
) -> Option<String> {
    let expected_identity = normalization_key.split_once(':').map(|(_, identity)| identity)?;

    rows.iter()
        .filter_map(|candidate| {
            let label = candidate.row.candidate_label.trim();
            if label.is_empty() {
                return None;
            }
            let label_identity = graph_identity::normalize_graph_identity_component(label);
            let exact_match = u8::from(label_identity == expected_identity);
            let word_like_bonus = u8::from(!label.contains('_'));
            let length = label.len();
            Some(((exact_match, word_like_bonus, length), label.to_string()))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, label)| label)
}

#[must_use]
fn canonical_label_from_node_key(canonical_key: &str) -> String {
    placeholder_entity_parts_from_key(canonical_key)
        .map(|(_, canonical_label)| canonical_label)
        .unwrap_or_default()
}

#[must_use]
fn runtime_node_type_from_candidate_type(candidate_type: &str) -> RuntimeNodeType {
    match candidate_type.trim() {
        "topic" => RuntimeNodeType::Topic,
        "document" => RuntimeNodeType::Document,
        _ => RuntimeNodeType::Entity,
    }
}

#[must_use]
fn build_materialized_extract_candidates(
    revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
    chunk_result: &repositories::extract_repository::ExtractChunkResultRow,
    node_candidates: &[repositories::extract_repository::ExtractNodeCandidateRow],
    edge_candidates: &[repositories::extract_repository::ExtractEdgeCandidateRow],
) -> MaterializedExtractCandidates {
    let mut display_labels_by_key = BTreeMap::<String, String>::new();
    let entity_candidates = node_candidates
        .iter()
        .filter_map(|candidate| {
            let display_label = candidate.display_label.trim();
            if display_label.is_empty() {
                return None;
            }
            let node_type = runtime_node_type_from_candidate_type(&candidate.node_kind);
            let candidate_type = graph_identity::runtime_node_type_slug(&node_type).to_string();
            let normalization_key = graph_identity::canonical_node_key(node_type, display_label);
            let display_label = display_label.to_string();
            if !candidate.canonical_key.trim().is_empty() {
                display_labels_by_key
                    .insert(candidate.canonical_key.trim().to_string(), display_label.clone());
            }
            display_labels_by_key.insert(normalization_key.clone(), display_label.clone());
            Some(NewKnowledgeEntityCandidate {
                candidate_id: candidate.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id: revision.revision_id,
                chunk_id: Some(chunk_result.chunk_id),
                candidate_label: display_label,
                candidate_type,
                normalization_key,
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
        })
        .collect::<Vec<_>>();

    let relation_candidates = edge_candidates
        .iter()
        .filter_map(|candidate| {
            let predicate = candidate.edge_kind.trim();
            if predicate.is_empty() {
                return None;
            }
            let subject_candidate_key = candidate.from_canonical_key.trim();
            let object_candidate_key = candidate.to_canonical_key.trim();
            let subject_display_label =
                display_labels_by_key.get(subject_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(subject_candidate_key)
                        .map(|(_, canonical_label)| canonical_label)
                })?;
            let object_display_label =
                display_labels_by_key.get(object_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(object_candidate_key)
                        .map(|(_, canonical_label)| canonical_label)
                })?;
            let normalized_subject_key = graph_identity::canonical_node_key(
                graph_identity::runtime_node_type_from_key(subject_candidate_key),
                &subject_display_label,
            );
            let normalized_object_key = graph_identity::canonical_node_key(
                graph_identity::runtime_node_type_from_key(object_candidate_key),
                &object_display_label,
            );
            Some(NewKnowledgeRelationCandidate {
                candidate_id: candidate.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id: revision.revision_id,
                chunk_id: Some(chunk_result.chunk_id),
                subject_label: subject_display_label,
                subject_candidate_key: normalized_subject_key.clone(),
                predicate: predicate.to_string(),
                object_label: object_display_label,
                object_candidate_key: normalized_object_key.clone(),
                normalized_assertion: canonical_relation_assertion_from_keys(
                    &normalized_subject_key,
                    predicate,
                    &normalized_object_key,
                ),
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
        })
        .collect::<Vec<_>>();

    MaterializedExtractCandidates { entity_candidates, relation_candidates }
}

#[must_use]
fn reconcile_entity_candidate_row(
    row: KnowledgeEntityCandidateRow,
    entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledEntityCandidate> {
    let trimmed_label = row.candidate_label.trim();
    if trimmed_label.is_empty() {
        return None;
    }
    Some(ReconciledEntityCandidate {
        normalization_key: entity_key_index.canonical_node_key_for_label(trimmed_label),
        row,
    })
}

#[must_use]
fn reconcile_relation_candidate_row(
    row: KnowledgeRelationCandidateRow,
    entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledRelationCandidate> {
    let predicate = row.predicate.trim().to_string();
    if predicate.is_empty() {
        return None;
    }

    let subject_candidate_key = if !row.subject_label.trim().is_empty() {
        entity_key_index.canonical_node_key_for_label(row.subject_label.trim())
    } else if let Some((_, canonical_label)) =
        placeholder_entity_parts_from_key(&row.subject_candidate_key)
    {
        entity_key_index.canonical_node_key_for_label(&canonical_label)
    } else {
        return None;
    };

    let object_candidate_key = if !row.object_label.trim().is_empty() {
        entity_key_index.canonical_node_key_for_label(row.object_label.trim())
    } else if let Some((_, canonical_label)) =
        placeholder_entity_parts_from_key(&row.object_candidate_key)
    {
        entity_key_index.canonical_node_key_for_label(&canonical_label)
    } else {
        return None;
    };

    Some(ReconciledRelationCandidate {
        normalized_assertion: canonical_relation_assertion_from_keys(
            &subject_candidate_key,
            &predicate,
            &object_candidate_key,
        ),
        row,
        subject_candidate_key,
        predicate,
        object_candidate_key,
    })
}

#[must_use]
fn relation_candidate_keys_are_materializable(
    subject_candidate_key: &str,
    predicate: &str,
    object_candidate_key: &str,
) -> bool {
    !normalize_evidence_literal(predicate).is_empty()
        && placeholder_entity_parts_from_key(subject_candidate_key).is_some()
        && placeholder_entity_parts_from_key(object_candidate_key).is_some()
}

#[cfg(test)]
fn canonical_entity_normalization_key(entity: &GraphEntityCandidate) -> String {
    graph_identity::canonical_node_key(entity.node_type.clone(), &entity.label)
}

#[cfg(test)]
#[must_use]
fn canonical_relation_assertion(relation: &GraphRelationCandidate) -> String {
    canonical_relation_assertion_from_keys(
        &graph_identity::canonical_node_key(RuntimeNodeType::Entity, &relation.source_label),
        &relation.relation_type,
        &graph_identity::canonical_node_key(RuntimeNodeType::Entity, &relation.target_label),
    )
}

#[must_use]
fn canonical_relation_assertion_from_keys(
    source_candidate_key: &str,
    relation_type: &str,
    target_candidate_key: &str,
) -> String {
    graph_identity::canonical_edge_key(source_candidate_key, relation_type, target_candidate_key)
}

#[must_use]
fn build_relation_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> graph_identity::GraphLabelNodeTypeIndex {
    let mut index = graph_identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type.clone());
    }
    index
}

#[must_use]
fn build_entity_candidate_key_index(
    candidates: &[KnowledgeEntityCandidateRow],
) -> graph_identity::GraphLabelNodeTypeIndex {
    let mut index = graph_identity::GraphLabelNodeTypeIndex::new();
    for candidate in candidates {
        let label = candidate.candidate_label.trim();
        if label.is_empty() {
            continue;
        }
        index.insert(label, runtime_node_type_from_candidate_type(&candidate.candidate_type));
    }
    index
}

#[must_use]
fn canonical_entity_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    normalization_key: &str,
    label: &str,
    node_type: &RuntimeNodeType,
) -> Uuid {
    stable_uuid(&format!(
        "arango-entity-candidate:{library_id}:{revision_id}:{chunk_id}:{normalization_key}:{label}:{}",
        graph_identity::runtime_node_type_slug(node_type)
    ))
}

#[must_use]
fn canonical_relation_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    normalized_assertion: &str,
    source_label: &str,
    target_label: &str,
    relation_type: &str,
) -> Uuid {
    stable_uuid(&format!(
        "arango-relation-candidate:{library_id}:{revision_id}:{chunk_id}:{normalized_assertion}:{source_label}:{target_label}:{relation_type}"
    ))
}

#[must_use]
fn canonical_entity_id(library_id: Uuid, normalization_key: &str) -> Uuid {
    stable_uuid(&format!("arango-entity:{library_id}:{normalization_key}"))
}

#[must_use]
fn canonical_relation_id(library_id: Uuid, normalized_assertion: &str) -> Uuid {
    stable_uuid(&format!("arango-relation:{library_id}:{normalized_assertion}"))
}

#[must_use]
fn canonical_evidence_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    support_kind: &str,
    canonical_key: &str,
) -> Uuid {
    stable_uuid(&format!(
        "arango-evidence:{library_id}:{revision_id}:{}:{support_kind}:{canonical_key}",
        chunk_id.map(|value| value.to_string()).unwrap_or_else(|| "none".to_string())
    ))
}

#[must_use]
fn stable_uuid(seed: &str) -> Uuid {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[must_use]
fn canonical_document_revision_edge_key(document_id: Uuid, revision_id: Uuid) -> String {
    format!("document:{document_id}:revision:{revision_id}")
}

#[must_use]
fn canonical_revision_chunk_edge_key(revision_id: Uuid, chunk_id: Uuid) -> String {
    format!("revision:{revision_id}:chunk:{chunk_id}")
}

#[must_use]
fn canonical_edge_relation_key(relation_id: Uuid, entity_id: Uuid, edge_kind: &str) -> String {
    format!("relation:{relation_id}:{edge_kind}:{entity_id}")
}

#[must_use]
fn canonical_chunk_mentions_entity_edge_key(chunk_id: Uuid, entity_id: Uuid) -> String {
    format!("chunk:{chunk_id}:mentions:{entity_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        infra::arangodb::{document_store::KnowledgeChunkRow, graph_store::GraphViewData},
        shared::technical_facts::{TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue},
    };

    #[test]
    fn merge_projection_data_prefers_incoming_canonical_rows() {
        let node_id = Uuid::now_v7();
        let edge_id = Uuid::now_v7();
        let merged = GraphService::merge_projection_data(
            &GraphViewData {
                nodes: vec![GraphViewNodeWrite {
                    node_id,
                    canonical_key: "entity:a".to_string(),
                    label: "A".to_string(),
                    node_type: "entity".to_string(),
                    support_count: 1,
                    summary: None,
                    aliases: vec![],
                    metadata_json: serde_json::json!({}),
                }],
                edges: vec![],
            },
            &GraphViewData {
                nodes: vec![GraphViewNodeWrite {
                    node_id,
                    canonical_key: "entity:a".to_string(),
                    label: "A2".to_string(),
                    node_type: "topic".to_string(),
                    support_count: 4,
                    summary: Some("updated".to_string()),
                    aliases: vec!["alias".to_string()],
                    metadata_json: serde_json::json!({"k": "v"}),
                }],
                edges: vec![GraphViewEdgeWrite {
                    edge_id,
                    from_node_id: node_id,
                    to_node_id: Uuid::now_v7(),
                    relation_type: "links_to".to_string(),
                    canonical_key: "entity:a--links_to--entity:b".to_string(),
                    support_count: 1,
                    summary: None,
                    weight: None,
                    metadata_json: serde_json::json!({}),
                }],
            },
        );

        assert_eq!(merged.nodes.len(), 1);
        assert_eq!(merged.nodes[0].label, "A2");
        assert_eq!(merged.nodes[0].support_count, 4);
        assert!(merged.edges.is_empty(), "dangling edge should be filtered");
    }

    #[test]
    fn relation_fields_are_semantically_empty_rejects_blank_members() {
        assert!(relation_fields_are_semantically_empty("", "supports", "beta"));
        assert!(relation_fields_are_semantically_empty("alpha", "supports", ""));
        assert!(!relation_fields_are_semantically_empty("alpha", "supports", "beta"));
    }

    #[test]
    fn rebuild_outcome_requires_materialized_entities_relations_or_evidence() {
        assert!(!ArangoGraphRebuildOutcome::default().has_materialized_graph());
        assert!(
            ArangoGraphRebuildOutcome { upserted_entities: 1, ..Default::default() }
                .has_materialized_graph()
        );
        assert!(
            ArangoGraphRebuildOutcome { upserted_relations: 1, ..Default::default() }
                .has_materialized_graph()
        );
        assert!(
            ArangoGraphRebuildOutcome { upserted_evidence: 1, ..Default::default() }
                .has_materialized_graph()
        );
    }

    #[test]
    fn placeholder_entity_parts_require_non_empty_suffix() {
        assert!(placeholder_entity_parts_from_key("entity:").is_none());
        assert_eq!(
            placeholder_entity_parts_from_key("entity:Первый_печатный_двор"),
            Some((RuntimeNodeType::Entity, "Первый_печатный_двор".to_string()))
        );
    }

    #[test]
    fn relation_candidate_keys_reject_dangling_entity_prefix() {
        assert!(!relation_candidate_keys_are_materializable("entity:acme", "supports", "entity:"));
        assert!(relation_candidate_keys_are_materializable(
            "entity:acme",
            "supports",
            "entity:касса"
        ));
    }

    #[test]
    fn reconcile_entity_candidate_row_recanonicalizes_legacy_unicode_key() {
        let mut entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
        entity_key_index.insert("Первый печатный двор", RuntimeNodeType::Entity);
        let row = KnowledgeEntityCandidateRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Some(Uuid::now_v7()),
            candidate_label: "Первый печатный двор".to_string(),
            candidate_type: "entity".to_string(),
            normalization_key: "entity:".to_string(),
            confidence: None,
            extraction_method: "extract_chunk_result".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let reconciled = reconcile_entity_candidate_row(row, &entity_key_index)
            .expect("entity candidate should reconcile");

        assert_eq!(reconciled.normalization_key, "entity:первый_печатный_двор");
    }

    #[test]
    fn reconcile_relation_candidate_row_uses_labels_to_rebuild_keys() {
        let entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
        let row = KnowledgeRelationCandidateRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Some(Uuid::now_v7()),
            subject_label: "Первый печатный двор".to_string(),
            subject_candidate_key: "entity:".to_string(),
            predicate: "mentions".to_string(),
            object_label: "Касса".to_string(),
            object_candidate_key: "topic:".to_string(),
            normalized_assertion: "entity:--legacy--topic:".to_string(),
            confidence: None,
            extraction_method: "extract_chunk_result".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let reconciled = reconcile_relation_candidate_row(row, &entity_key_index)
            .expect("relation candidate should reconcile");

        assert_eq!(reconciled.subject_candidate_key, "entity:первый_печатный_двор");
        assert_eq!(reconciled.object_candidate_key, "entity:касса");
        assert_eq!(
            reconciled.normalized_assertion,
            "entity:первый_печатный_двор--mentions--entity:касса"
        );
    }

    #[test]
    fn reconcile_relation_candidate_row_rejects_missing_identity_without_labels() {
        let entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
        let row = KnowledgeRelationCandidateRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Some(Uuid::now_v7()),
            subject_label: String::new(),
            subject_candidate_key: "entity:".to_string(),
            predicate: "supports".to_string(),
            object_label: String::new(),
            object_candidate_key: "entity:acme".to_string(),
            normalized_assertion: "entity:--supports--entity:acme".to_string(),
            confidence: None,
            extraction_method: "extract_chunk_result".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(reconcile_relation_candidate_row(row, &entity_key_index).is_none());
    }

    #[test]
    fn reconcile_entity_candidate_row_prefers_entity_for_label_type_collisions() {
        let mut entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
        entity_key_index.insert("Касса", RuntimeNodeType::Topic);
        entity_key_index.insert("Касса", RuntimeNodeType::Entity);
        let row = KnowledgeEntityCandidateRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            candidate_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Some(Uuid::now_v7()),
            candidate_label: "Касса".to_string(),
            candidate_type: "topic".to_string(),
            normalization_key: "topic:касса".to_string(),
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let reconciled = reconcile_entity_candidate_row(row, &entity_key_index)
            .expect("entity candidate should reconcile");

        assert_eq!(reconciled.normalization_key, "entity:касса");
    }

    #[test]
    fn build_prefixed_entity_key_aliases_collapses_unbranded_product_keys() {
        let revision_id = Uuid::now_v7();
        let branded = ReconciledEntityCandidate {
            normalization_key: "entity:acme_control_center".to_string(),
            row: KnowledgeEntityCandidateRow {
                key: Uuid::now_v7().to_string(),
                arango_id: None,
                arango_rev: None,
                candidate_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                revision_id,
                chunk_id: Some(Uuid::now_v7()),
                candidate_label: "Acme Control Center".to_string(),
                candidate_type: "entity".to_string(),
                normalization_key: "entity:acme_control_center".to_string(),
                confidence: None,
                extraction_method: "graph_extract".to_string(),
                candidate_state: "active".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        };
        let unbranded = ReconciledEntityCandidate {
            normalization_key: "entity:control_center".to_string(),
            row: KnowledgeEntityCandidateRow {
                key: Uuid::now_v7().to_string(),
                arango_id: None,
                arango_rev: None,
                candidate_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                revision_id,
                chunk_id: Some(Uuid::now_v7()),
                candidate_label: "Control Center".to_string(),
                candidate_type: "entity".to_string(),
                normalization_key: "entity:control_center".to_string(),
                confidence: None,
                extraction_method: "graph_extract".to_string(),
                candidate_state: "active".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        };

        let aliases = build_prefixed_entity_key_aliases(&[branded, unbranded]);

        assert_eq!(
            aliases.get("entity:control_center"),
            Some(&"entity:acme_control_center".to_string())
        );
    }

    #[test]
    fn apply_entity_key_aliases_to_relation_candidate_rebuilds_assertion() {
        let mut candidate = ReconciledRelationCandidate {
            row: KnowledgeRelationCandidateRow {
                key: Uuid::now_v7().to_string(),
                arango_id: None,
                arango_rev: None,
                candidate_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_id: Some(Uuid::now_v7()),
                subject_label: "Control Center".to_string(),
                subject_candidate_key: "entity:control_center".to_string(),
                predicate: "manages".to_string(),
                object_label: "Касса".to_string(),
                object_candidate_key: "entity:касса".to_string(),
                normalized_assertion: "entity:control_center--manages--entity:касса".to_string(),
                confidence: None,
                extraction_method: "graph_extract".to_string(),
                candidate_state: "active".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            subject_candidate_key: "entity:control_center".to_string(),
            predicate: "manages".to_string(),
            object_candidate_key: "entity:касса".to_string(),
            normalized_assertion: "entity:control_center--manages--entity:касса".to_string(),
        };

        apply_entity_key_aliases_to_relation_candidate(
            &mut candidate,
            &BTreeMap::from([(
                "entity:control_center".to_string(),
                "entity:acme_control_center".to_string(),
            )]),
        );

        assert_eq!(candidate.subject_candidate_key, "entity:acme_control_center");
        assert_eq!(
            candidate.normalized_assertion,
            "entity:acme_control_center--manages--entity:касса"
        );
    }

    #[test]
    fn build_materialized_extract_candidates_recanonicalizes_unicode_node_rows() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let revision = crate::infra::arangodb::document_store::KnowledgeRevisionRow {
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id,
            workspace_id,
            library_id,
            document_id: Uuid::now_v7(),
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "source".to_string(),
            storage_ref: None,
            source_uri: None,
            mime_type: "text/plain".to_string(),
            checksum: "checksum".to_string(),
            title: None,
            byte_size: 1,
            normalized_text: Some("text".to_string()),
            text_checksum: Some("checksum".to_string()),
            text_state: "readable".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "ready".to_string(),
            text_readable_at: Some(Utc::now()),
            vector_ready_at: Some(Utc::now()),
            graph_ready_at: Some(Utc::now()),
            superseded_by_revision_id: None,
            created_at: Utc::now(),
        };
        let chunk_result = repositories::extract_repository::ExtractChunkResultRow {
            id: Uuid::now_v7(),
            chunk_id,
            attempt_id: Uuid::now_v7(),
            extract_state: "ready".to_string(),
            provider_call_id: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            failure_code: None,
        };
        let node_rows = vec![repositories::extract_repository::ExtractNodeCandidateRow {
            id: Uuid::now_v7(),
            chunk_result_id: chunk_result.id,
            canonical_key: "entity:".to_string(),
            node_kind: "entity".to_string(),
            display_label: "Первый печатный двор".to_string(),
            summary: None,
        }];

        let materialized =
            build_materialized_extract_candidates(&revision, &chunk_result, &node_rows, &[]);

        assert_eq!(materialized.entity_candidates.len(), 1);
        assert_eq!(
            materialized.entity_candidates[0].normalization_key,
            "entity:первый_печатный_двор"
        );
        assert_eq!(materialized.entity_candidates[0].candidate_label, "Первый печатный двор");
    }

    #[test]
    fn build_materialized_extract_candidates_derives_relation_labels_from_nodes() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let revision = crate::infra::arangodb::document_store::KnowledgeRevisionRow {
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id,
            workspace_id,
            library_id,
            document_id: Uuid::now_v7(),
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "source".to_string(),
            storage_ref: None,
            source_uri: None,
            mime_type: "text/plain".to_string(),
            checksum: "checksum".to_string(),
            title: None,
            byte_size: 1,
            normalized_text: Some("text".to_string()),
            text_checksum: Some("checksum".to_string()),
            text_state: "readable".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "ready".to_string(),
            text_readable_at: Some(Utc::now()),
            vector_ready_at: Some(Utc::now()),
            graph_ready_at: Some(Utc::now()),
            superseded_by_revision_id: None,
            created_at: Utc::now(),
        };
        let chunk_result = repositories::extract_repository::ExtractChunkResultRow {
            id: Uuid::now_v7(),
            chunk_id,
            attempt_id: Uuid::now_v7(),
            extract_state: "ready".to_string(),
            provider_call_id: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            failure_code: None,
        };
        let node_rows = vec![
            repositories::extract_repository::ExtractNodeCandidateRow {
                id: Uuid::now_v7(),
                chunk_result_id: chunk_result.id,
                canonical_key: "entity:".to_string(),
                node_kind: "entity".to_string(),
                display_label: "Первый печатный двор".to_string(),
                summary: None,
            },
            repositories::extract_repository::ExtractNodeCandidateRow {
                id: Uuid::now_v7(),
                chunk_result_id: chunk_result.id,
                canonical_key: "topic:касса".to_string(),
                node_kind: "topic".to_string(),
                display_label: "Касса".to_string(),
                summary: None,
            },
        ];
        let edge_rows = vec![repositories::extract_repository::ExtractEdgeCandidateRow {
            id: Uuid::now_v7(),
            chunk_result_id: chunk_result.id,
            canonical_key: "entity:--mentions--topic:касса".to_string(),
            edge_kind: "mentions".to_string(),
            from_canonical_key: "entity:".to_string(),
            to_canonical_key: "topic:касса".to_string(),
            summary: None,
        }];

        let materialized =
            build_materialized_extract_candidates(&revision, &chunk_result, &node_rows, &edge_rows);

        assert_eq!(materialized.relation_candidates.len(), 1);
        let relation = &materialized.relation_candidates[0];
        assert_eq!(relation.subject_label, "Первый печатный двор");
        assert_eq!(relation.object_label, "Касса");
        assert_eq!(relation.subject_candidate_key, "entity:первый_печатный_двор");
        assert_eq!(relation.object_candidate_key, "topic:касса");
        assert_eq!(
            relation.normalized_assertion,
            "entity:первый_печатный_двор--mentions--topic:касса"
        );
    }

    #[test]
    fn canonical_entity_normalization_key_preserves_unicode_and_node_type() {
        let entity = GraphEntityCandidate {
            label: "Первый печатный двор".to_string(),
            node_type: RuntimeNodeType::Entity,
            aliases: vec![],
            summary: None,
        };
        let topic = GraphEntityCandidate {
            label: "Первый печатный двор".to_string(),
            node_type: RuntimeNodeType::Topic,
            aliases: vec![],
            summary: None,
        };

        assert_eq!(canonical_entity_normalization_key(&entity), "entity:первый_печатный_двор");
        assert_eq!(canonical_entity_normalization_key(&topic), "topic:первый_печатный_двор");
    }

    #[test]
    fn canonical_relation_assertion_preserves_unicode_entity_keys() {
        let relation = GraphRelationCandidate {
            source_label: "Acme Касса".to_string(),
            target_label: "Первый печатный двор".to_string(),
            relation_type: "part_of".to_string(),
            summary: None,
        };

        assert_eq!(
            canonical_relation_assertion(&relation),
            "entity:acme_касса--part_of--entity:первый_печатный_двор"
        );
    }

    #[test]
    fn resolve_entity_evidence_support_prefers_matching_fact_support() {
        let block_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let fact_id = Uuid::now_v7();
        let chunk = KnowledgeChunkRow {
            key: chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("endpoint_block".to_string()),
            content_text: "GET /api/status".to_string(),
            normalized_text: "GET /api/status".to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: vec![block_id],
            section_path: vec!["API".to_string()],
            heading_trail: vec!["Status".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
        };
        let fact = TypedTechnicalFact {
            fact_id,
            revision_id: chunk.revision_id,
            document_id: chunk.document_id,
            workspace_id: chunk.workspace_id,
            library_id: chunk.library_id,
            fact_kind: TechnicalFactKind::EndpointPath,
            canonical_value: TechnicalFactValue::Text("/api/status".to_string()),
            display_value: "/api/status".to_string(),
            qualifiers: vec![TechnicalFactQualifier {
                key: "method".to_string(),
                value: "GET".to_string(),
            }],
            support_block_ids: vec![block_id],
            support_chunk_ids: vec![chunk_id],
            confidence: Some(0.91),
            extraction_kind: "parser".to_string(),
            conflict_group_id: None,
            created_at: Utc::now(),
        };

        let support =
            resolve_entity_evidence_support("/api/status", "/api/status", Some(&chunk), &[fact]);
        assert_eq!(support.block_id, Some(block_id));
        assert_eq!(support.fact_id, Some(fact_id));
        assert_eq!(support.evidence_kind, "entity_fact_support");
        assert_eq!(support.literal_spans.len(), 1);
    }
}
