use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::infra::knowledge_rows::{
    GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
    KnowledgeEntityCandidateRow, KnowledgeEntityRow, KnowledgeEvidenceRow,
    KnowledgeGraphTraversalRow, KnowledgeRelationCandidateRow, KnowledgeRelationEvidenceLookupRow,
    KnowledgeRelationRow, KnowledgeRelationTopologyRow, NewKnowledgeEntity,
    NewKnowledgeEntityCandidate, NewKnowledgeEvidence, NewKnowledgeRelation,
    NewKnowledgeRelationCandidate,
};

/// Shared handle for a [`GraphStore`] adapter.
pub type GraphStoreRef = Arc<dyn GraphStore>;

/// Ranking and ownership metadata shared by the structural edge upserts.
pub struct EdgeRanking {
    pub rank: Option<i32>,
    pub score: Option<f64>,
    pub inclusion_reason: Option<String>,
    pub library_id: Uuid,
}

/// Optional edge targets written together with a materialized evidence row.
pub struct EvidenceEdgeTargets {
    pub source_revision_id: Option<Uuid>,
    pub supporting_entity_id: Option<Uuid>,
    pub supporting_relation_id: Option<Uuid>,
    pub supporting_fact_id: Option<Uuid>,
    pub library_id: Uuid,
}

/// Full replacement payload for one library's materialized graph projection.
pub struct LibraryProjectionTargets<'a> {
    pub library_id: Uuid,
    pub projection_version: i64,
    pub remove_node_ids: &'a [Uuid],
    pub remove_edge_ids: &'a [Uuid],
    pub nodes: &'a [GraphViewNodeWrite],
    pub edges: &'a [GraphViewEdgeWrite],
}

/// Materialized graph: entities, relations, evidence, their
/// candidates, the structural edges, and the depth-capped traversal reads.
#[async_trait]
pub trait GraphStore: Send + Sync {
    async fn ping(&self) -> anyhow::Result<()>;

    // --- candidates ---
    async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow>;
    async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow>;
    async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;

    // --- structural edges / projection ---
    async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64>;
    async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()>;
    async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    /// Upserts the evidence→source structural edge. Required direction is
    /// **EVIDENCE→REVISION** (`_from`=evidence, `_to`=revision); the stale
    /// bootstrap EVIDENCE→CHUNK declaration does not match what is written. The
    /// PG adapter enforces EVIDENCE→REVISION with an FK. (Doc pin only — no
    /// runtime change in this refactor.)
    async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()>;
    async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()>;
    async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn replace_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
        nodes: &[GraphViewNodeWrite],
        edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError>;
    async fn refresh_library_projection_targets(
        &self,
        targets: LibraryProjectionTargets<'_>,
    ) -> Result<(), GraphViewWriteError>;
    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphViewData>;

    // --- materialized entities / relations / evidence ---
    async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        targets: EvidenceEdgeTargets,
    ) -> anyhow::Result<KnowledgeEvidenceRow>;
    async fn upsert_entity(&self, input: &NewKnowledgeEntity)
    -> anyhow::Result<KnowledgeEntityRow>;
    async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>>;
    async fn get_entity_by_id(&self, entity_id: Uuid)
    -> anyhow::Result<Option<KnowledgeEntityRow>>;
    async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        primary_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>>;
    async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>>;
    async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow>;
    async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeRelationRow>;
    async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>>;
    async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>>;
    async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>>;
    async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>>;
    /// Returns the number of entities removed (§14.4(b)).
    async fn delete_entities_by_identity_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64>;
    /// Returns the number of relations removed (§14.4(b)).
    async fn delete_relations_by_identity_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64>;
    async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow>;
    async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;

    // --- traversal reads (depth-capped) ---
    async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>>;
    async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>>;
    async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>>;
    async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>>;
    async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>>;
}
