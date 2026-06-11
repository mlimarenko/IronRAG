//! Knowledge-plane [`GraphStore`] port implementation for the Arango
//! adapter. Each method forwards verbatim to the inherent
//! `ArangoGraphStore` method (same AQL, same logic). The trait is the
//! swappable boundary; the bodies stay on the concrete struct (spread
//! across the `candidates`/`edges_or_projection`/`materialized`/`traversal`
//! submodules).
//!
//! `evidence_source_edge` canonical direction is EVIDENCE→REVISION — see
//! [`crate::infra::knowledge_plane::GraphStore::upsert_evidence_source_edge`].
#![allow(clippy::too_many_arguments)]

use super::*;

#[async_trait::async_trait]
impl crate::infra::knowledge_plane::GraphStore for ArangoGraphStore {
    async fn ping(&self) -> anyhow::Result<()> {
        self.ping().await
    }
    async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow> {
        self.upsert_entity_candidate(input).await
    }
    async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.upsert_entity_candidates(inputs).await
    }
    async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.list_entity_candidates_by_revision(revision_id).await
    }
    async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.list_entity_candidates_by_library(library_id).await
    }
    async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.delete_entity_candidates_by_revision(revision_id).await
    }
    async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.delete_entity_candidates_by_library(library_id).await
    }
    async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow> {
        self.upsert_relation_candidate(input).await
    }
    async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.upsert_relation_candidates(inputs).await
    }
    async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.list_relation_candidates_by_revision(revision_id).await
    }
    async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.list_relation_candidates_by_library(library_id).await
    }
    async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.delete_relation_candidates_by_revision(revision_id).await
    }
    async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.delete_relation_candidates_by_library(library_id).await
    }
    async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_document_revision_edge(document_id, revision_id, library_id).await
    }
    async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_revision_chunk_edge(revision_id, chunk_id, library_id).await
    }
    async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_revision_chunk_edges(revision_id, chunk_ids, library_id).await
    }
    async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64> {
        self.delete_revision_chunk_edges(revision_id).await
    }
    async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_chunk_mentions_entity_edge(
            chunk_id,
            entity_id,
            rank,
            score,
            inclusion_reason,
            library_id,
        )
        .await
    }
    async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_relation_subject_edge(relation_id, subject_entity_id, library_id).await
    }
    async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_relation_object_edge(relation_id, object_entity_id, library_id).await
    }
    async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_source_edge(evidence_id, revision_id, library_id).await
    }
    async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_supports_entity_edge(
            evidence_id,
            entity_id,
            rank,
            score,
            inclusion_reason,
            library_id,
        )
        .await
    }
    async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_supports_relation_edge(
            evidence_id,
            relation_id,
            rank,
            score,
            inclusion_reason,
            library_id,
        )
        .await
    }
    async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_fact_supports_evidence_edge(fact_id, evidence_id, library_id).await
    }
    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        self.replace_library_projection(_library_id, _projection_version, _nodes, _edges).await
    }
    async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        self.refresh_library_projection_targets(
            _library_id,
            _projection_version,
            _remove_node_ids,
            _remove_edge_ids,
            _nodes,
            _edges,
        )
        .await
    }
    async fn load_library_projection(
        &self,
        library_id: Uuid,
        _projection_version: i64,
    ) -> anyhow::Result<GraphViewData> {
        self.load_library_projection(library_id, _projection_version).await
    }
    async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
        supporting_fact_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        self.upsert_evidence_with_edges(
            input,
            source_revision_id,
            supporting_entity_id,
            supporting_relation_id,
            supporting_fact_id,
            library_id,
        )
        .await
    }
    async fn reset_library_materialized_graph(&self, library_id: Uuid) -> anyhow::Result<()> {
        self.reset_library_materialized_graph(library_id).await
    }
    async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        self.upsert_entity(input).await
    }
    async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        self.upsert_entities(inputs).await
    }
    async fn get_entity_by_id(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        self.get_entity_by_id(entity_id).await
    }
    async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        canonical_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        self.get_entity_by_library_and_label(library_id, canonical_label).await
    }
    async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        self.list_entities_by_library(library_id).await
    }
    async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        self.upsert_relation(input).await
    }
    async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        self.upsert_relation_with_endpoints(input, subject_entity_id, object_entity_id, library_id)
            .await
    }
    async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        self.upsert_relations(inputs).await
    }
    async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        self.get_relation_by_id(relation_id).await
    }
    async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        self.get_relation_by_library_and_assertion(library_id, normalized_assertion).await
    }
    async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        self.list_relations_by_library(library_id).await
    }
    async fn delete_entities_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        self.delete_entities_by_canonical_keys(library_id, keys).await
    }
    async fn delete_relations_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        self.delete_relations_by_canonical_keys(library_id, keys).await
    }
    async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        self.upsert_evidence(input).await
    }
    async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        self.get_evidence_by_id(evidence_id).await
    }
    async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_ids(evidence_ids).await
    }
    async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_revision(revision_id).await
    }
    async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_chunk(chunk_id).await
    }
    async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        self.list_relation_topology_by_library(library_id).await
    }
    async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        self.get_relation_topology_by_id(relation_id).await
    }
    async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.list_entity_neighborhood(entity_id, library_id, max_depth, limit).await
    }
    async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.expand_relation_centric(relation_id, library_id, max_depth, limit).await
    }
    async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        self.list_relation_evidence_lookup(relation_id, library_id, limit).await
    }
}
