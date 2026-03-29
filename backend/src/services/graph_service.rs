use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::runtime_graph::RuntimeNodeType,
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
        graph_merge::{self, GraphMergeOutcome, GraphMergeScope},
        graph_projection::{self, GraphProjectionOutcome, GraphProjectionScope},
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

        self.materialize_current_candidate_batch(state, &revision, chunk_id, candidates, false)
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
                    span_start: evidence.span_start,
                    span_end: evidence.span_end,
                    excerpt: evidence.excerpt,
                    support_kind: evidence.support_kind,
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
                if revision.text_state == "readable" {
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

    pub async fn rebuild_arango_library_graph(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.with_runtime_graph_lock(state, library_id, async {
            let mut outcome =
                self.reconcile_arango_library_candidates(state, library_id, None).await?;
            outcome.target = Some(ArangoGraphRebuildTarget::Graph);
            self.recalculate_arango_library_generations(state, library_id)
                .await
                .context("failed to refresh arango generation state after graph rebuild")?;
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
            let vector = self.rebuild_arango_library_vector(state, library_id).await?;
            let graph = self.reconcile_arango_library_candidates(state, library_id, None).await?;
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

    async fn with_runtime_graph_lock<F>(
        &self,
        state: &AppState,
        library_id: Uuid,
        operation: F,
    ) -> Result<ArangoGraphRebuildOutcome>
    where
        F: std::future::Future<Output = Result<ArangoGraphRebuildOutcome>>,
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
            candidates: Vec<KnowledgeRelationCandidateRow>,
            relation_id: Uuid,
        }

        let mut revision_contexts = BTreeMap::<Uuid, ArangoRevisionContext>::new();
        for revision_id in entity_candidates
            .iter()
            .map(|row| row.revision_id)
            .chain(relation_candidates.iter().map(|row| row.revision_id))
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

        let mut outcome = ArangoGraphRebuildOutcome {
            scanned_entity_candidates: entity_candidates.len(),
            scanned_relation_candidates: relation_candidates.len(),
            ..Default::default()
        };

        let mut entity_groups = BTreeMap::<String, Vec<KnowledgeEntityCandidateRow>>::new();
        for row in entity_candidates {
            entity_groups.entry(row.normalization_key.clone()).or_default().push(row);
        }

        let mut entity_reconcile_groups = Vec::<EntityReconcileGroup>::new();
        let mut entity_requests = Vec::<NewKnowledgeEntity>::new();
        let mut entity_request_ids = BTreeSet::<Uuid>::new();
        for (normalization_key, rows) in entity_groups {
            let row = rows.last().expect("entity candidate group is non-empty");
            let revision_context = revision_contexts.get(&row.revision_id).ok_or_else(|| {
                anyhow::anyhow!("missing revision context for {}", row.revision_id)
            })?;
            let canonical_label = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.candidate_label.trim().is_empty())
                        .then(|| candidate.candidate_label.trim().to_string())
                })
                .unwrap_or_else(|| normalization_key.clone());
            let entity_type = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.candidate_type.trim().is_empty())
                        .then(|| candidate.candidate_type.trim().to_string())
                })
                .unwrap_or_else(|| "entity".to_string());
            let aliases = self.collect_entity_aliases(
                &rows,
                alias_overrides,
                &normalization_key,
                &canonical_label,
            );
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let entity_id = canonical_entity_id(library_id, &normalization_key, &entity_type);
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
                candidates: rows,
                entity_id,
            });
        }

        let mut relation_groups = BTreeMap::<String, Vec<KnowledgeRelationCandidateRow>>::new();
        for row in relation_candidates {
            relation_groups.entry(row.normalized_assertion.clone()).or_default().push(row);
        }

        let mut relation_reconcile_groups = Vec::<RelationReconcileGroup>::new();
        let mut relation_requests = Vec::<NewKnowledgeRelation>::new();
        let mut placeholder_entity_requests = BTreeMap::<Uuid, NewKnowledgeEntity>::new();
        for (normalized_assertion, rows) in relation_groups {
            let row = rows.last().expect("relation candidate group is non-empty");
            let revision_context = revision_contexts.get(&row.revision_id).ok_or_else(|| {
                anyhow::anyhow!("missing revision context for {}", row.revision_id)
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
                .filter_map(|candidate| candidate.confidence)
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
                    let entity_id = canonical_entity_id(library_id, normalization_key, "entity");
                    if entity_request_ids.contains(&entity_id) {
                        continue;
                    }
                    let entry = placeholder_entity_requests.entry(entity_id).or_insert_with(|| {
                        NewKnowledgeEntity {
                            entity_id,
                            workspace_id: revision_context.workspace_id,
                            library_id,
                            canonical_label: normalization_key.trim().to_string(),
                            aliases: vec![normalization_key.trim().to_string()],
                            entity_type: "entity".to_string(),
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
        let entity_rows = state.arango_graph_store.upsert_entities(&entity_requests).await?;
        let entity_by_id =
            entity_rows.into_iter().map(|row| (row.entity_id, row)).collect::<BTreeMap<_, _>>();

        for group in entity_reconcile_groups {
            let entity = entity_by_id.get(&group.entity_id).ok_or_else(|| {
                anyhow::anyhow!("missing canonical entity {} after bulk upsert", group.entity_id)
            })?;
            outcome.upserted_entities += 1;
            for candidate in group.candidates {
                self.upsert_current_entity_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    entity,
                    &group.normalization_key,
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
                let subject_id =
                    canonical_entity_id(library_id, &candidate.subject_candidate_key, "entity");
                let object_id =
                    canonical_entity_id(library_id, &candidate.object_candidate_key, "entity");
                let subject = entity_by_id.get(&subject_id).ok_or_else(|| {
                    anyhow::anyhow!("missing subject placeholder entity {}", subject_id)
                })?;
                let object = entity_by_id.get(&object_id).ok_or_else(|| {
                    anyhow::anyhow!("missing object placeholder entity {}", object_id)
                })?;
                self.upsert_relation_edges(state, relation, subject, object).await?;
                self.upsert_current_relation_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    relation,
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_relation_subject_edges += 1;
                outcome.upserted_relation_object_edges += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_relation_edges += 1;
            }
        }

        for revision_context in revision_contexts.values() {
            self.upsert_revision_edges(state, revision_context).await?;
            outcome.upserted_document_revision_edges += 1;
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
        let entity_alias_overrides = self.build_alias_overrides(candidates);
        self.upsert_revision_edges(state, &revision_context).await?;
        self.upsert_chunk_edge(state, &revision_context, chunk_id).await?;

        for entity in &candidates.entities {
            let candidate = self.build_entity_candidate_row(revision, chunk_id, entity);
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
            let candidate = self.build_relation_candidate_row(revision, chunk_id, relation);
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
                    .upsert_placeholder_entity_for_label(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.subject_candidate_key,
                    )
                    .await?;
                let object = self
                    .upsert_placeholder_entity_for_label(
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
                )
                .await?;
            }
        }

        Ok(())
    }

    fn build_alias_overrides(
        &self,
        candidates: &GraphExtractionCandidateSet,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut overrides = BTreeMap::<String, BTreeSet<String>>::new();
        for entity in &candidates.entities {
            let key = canonical_entity_normalization_key(entity);
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
    ) -> NewKnowledgeEntityCandidate {
        let normalization_key = canonical_entity_normalization_key(entity);
        let candidate_id = canonical_entity_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalization_key,
            &entity.label,
            &entity.node_type,
        );
        NewKnowledgeEntityCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            candidate_label: entity.label.trim().to_string(),
            candidate_type: runtime_node_type_slug(&entity.node_type).to_string(),
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
    ) -> NewKnowledgeRelationCandidate {
        let normalized_assertion = canonical_relation_assertion(relation);
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
            subject_candidate_key: canonical_entity_normalization_key_from_label(
                &relation.source_label,
            ),
            predicate: relation.relation_type.trim().to_string(),
            object_candidate_key: canonical_entity_normalization_key_from_label(
                &relation.target_label,
            ),
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
        let entity_id = canonical_entity_id(library_id, normalization_key, entity_type);
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
        Err(last_err.unwrap()).context("failed to upsert canonical arango entity after retries")
    }

    async fn upsert_placeholder_entity_for_label(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        label_key: &str,
    ) -> Result<KnowledgeEntityRow> {
        let canonical_label = label_key.trim();
        let aliases = {
            let mut set = BTreeSet::new();
            if !canonical_label.is_empty() {
                set.insert(canonical_label.to_string());
            }
            set
        };
        self.upsert_canonical_entity(
            state,
            library_id,
            workspace_id,
            &canonical_entity_normalization_key_from_label(canonical_label),
            canonical_label,
            "entity",
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
        Err(last_err.unwrap()).context("failed to upsert canonical arango relation after retries")
    }

    async fn upsert_current_entity_evidence<C>(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        candidate: &C,
        entity: &KnowledgeEntityRow,
        canonical_key: &str,
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
        if let Some(existing) = state
            .arango_graph_store
            .get_evidence_by_id(evidence_id)
            .await
            .context("failed to load arango entity evidence before upsert")?
        {
            return Ok(existing);
        }
        let excerpt = candidate.candidate_label().to_string();
        let row = state
            .arango_graph_store
            .upsert_evidence(&NewKnowledgeEvidence {
                evidence_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id: revision.revision_id,
                chunk_id: candidate.chunk_id(),
                span_start: None,
                span_end: None,
                excerpt,
                support_kind: "entity_candidate".to_string(),
                extraction_method: candidate.extraction_method().to_string(),
                confidence: candidate.confidence(),
                evidence_state: "active".to_string(),
                freshness_generation: revision.revision_number,
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await
            .context("failed to upsert arango entity evidence")?;
        self.upsert_arango_edge(
            state,
            "knowledge_evidence_source_edge",
            canonical_evidence_source_edge_key(row.evidence_id, revision.revision_id),
            "knowledge_evidence",
            row.evidence_id,
            "knowledge_revision",
            revision.revision_id,
            json!({}),
        )
        .await?;
        self.upsert_arango_edge(
            state,
            "knowledge_evidence_supports_entity_edge",
            canonical_evidence_support_entity_edge_key(row.evidence_id, entity.entity_id),
            "knowledge_evidence",
            row.evidence_id,
            "knowledge_entity",
            entity.entity_id,
            json!({
                "rank": 1,
                "score": candidate.confidence(),
                "inclusionReason": "graph_extract_entity_candidate",
            }),
        )
        .await?;
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
        if let Some(existing) = state
            .arango_graph_store
            .get_evidence_by_id(evidence_id)
            .await
            .context("failed to load arango relation evidence before upsert")?
        {
            return Ok(existing);
        }
        let excerpt = format!(
            "{} {} {}",
            candidate.subject_candidate_key(),
            candidate.predicate(),
            candidate.object_candidate_key()
        );
        let row = state
            .arango_graph_store
            .upsert_evidence(&NewKnowledgeEvidence {
                evidence_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id: revision.revision_id,
                chunk_id: candidate.chunk_id(),
                span_start: None,
                span_end: None,
                excerpt,
                support_kind: "relation_candidate".to_string(),
                extraction_method: candidate.extraction_method().to_string(),
                confidence: candidate.confidence(),
                evidence_state: "active".to_string(),
                freshness_generation: revision.revision_number,
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await
            .context("failed to upsert arango relation evidence")?;
        self.upsert_arango_edge(
            state,
            "knowledge_evidence_source_edge",
            canonical_evidence_source_edge_key(row.evidence_id, revision.revision_id),
            "knowledge_evidence",
            row.evidence_id,
            "knowledge_revision",
            revision.revision_id,
            json!({}),
        )
        .await?;
        self.upsert_arango_edge(
            state,
            "knowledge_evidence_supports_relation_edge",
            canonical_evidence_support_relation_edge_key(row.evidence_id, relation.relation_id),
            "knowledge_evidence",
            row.evidence_id,
            "knowledge_relation",
            relation.relation_id,
            json!({
                "rank": 1,
                "score": candidate.confidence(),
                "inclusionReason": "graph_extract_relation_candidate",
            }),
        )
        .await?;
        let subject = self
            .upsert_placeholder_entity_for_label(
                state,
                revision.library_id,
                revision.workspace_id,
                candidate.subject_candidate_key(),
            )
            .await?;
        let object = self
            .upsert_placeholder_entity_for_label(
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
        Err(last_err.unwrap())
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
                if revision.text_state == "readable" {
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

#[must_use]
fn runtime_node_type_slug(node_type: &RuntimeNodeType) -> &'static str {
    match node_type {
        RuntimeNodeType::Document => "document",
        RuntimeNodeType::Entity => "entity",
        RuntimeNodeType::Topic => "topic",
    }
}

#[must_use]
fn canonical_entity_normalization_key(entity: &GraphEntityCandidate) -> String {
    canonical_entity_normalization_key_from_label(&entity.label)
}

#[must_use]
fn canonical_entity_normalization_key_from_label(label: &str) -> String {
    graph_merge::canonical_node_key(RuntimeNodeType::Entity, label)
}

#[must_use]
fn canonical_relation_assertion(relation: &GraphRelationCandidate) -> String {
    graph_merge::canonical_edge_key(
        &canonical_entity_normalization_key_from_label(&relation.source_label),
        &relation.relation_type,
        &canonical_entity_normalization_key_from_label(&relation.target_label),
    )
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
        runtime_node_type_slug(node_type)
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
fn canonical_entity_id(library_id: Uuid, normalization_key: &str, entity_type: &str) -> Uuid {
    stable_uuid(&format!("arango-entity:{library_id}:{entity_type}:{normalization_key}"))
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
fn canonical_evidence_source_edge_key(evidence_id: Uuid, revision_id: Uuid) -> String {
    format!("evidence:{evidence_id}:source:{revision_id}")
}

#[must_use]
fn canonical_evidence_support_entity_edge_key(evidence_id: Uuid, entity_id: Uuid) -> String {
    format!("evidence:{evidence_id}:supports_entity:{entity_id}")
}

#[must_use]
fn canonical_evidence_support_relation_edge_key(evidence_id: Uuid, relation_id: Uuid) -> String {
    format!("evidence:{evidence_id}:supports_relation:{relation_id}")
}

#[must_use]
fn canonical_chunk_mentions_entity_edge_key(chunk_id: Uuid, entity_id: Uuid) -> String {
    format!("chunk:{chunk_id}:mentions:{entity_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::arangodb::graph_store::GraphViewData;

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
}
