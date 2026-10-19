use anyhow::Context;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::{EdgeRanking, EvidenceEdgeTargets, GraphStore, LibraryProjectionTargets},
    knowledge_rows::{
        GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
        KnowledgeEntityCandidateRow, KnowledgeEntityRow, KnowledgeEvidenceRow,
        KnowledgeGraphTraversalRow, KnowledgeRelationCandidateRow,
        KnowledgeRelationEvidenceLookupRow, KnowledgeRelationRow, KnowledgeRelationTopologyRow,
        NewKnowledgeEntity, NewKnowledgeEntityCandidate, NewKnowledgeEvidence,
        NewKnowledgeRelation, NewKnowledgeRelationCandidate,
    },
};

const KNOWLEDGE_CHUNK_COLLECTION: &str = "knowledge_chunk";
const KNOWLEDGE_DOCUMENT_COLLECTION: &str = "knowledge_document";
const KNOWLEDGE_ENTITY_COLLECTION: &str = "knowledge_entity";
const KNOWLEDGE_EVIDENCE_COLLECTION: &str = "knowledge_evidence";
const KNOWLEDGE_RELATION_COLLECTION: &str = "knowledge_relation";
const ENTITY_CANDIDATE_ROW_SQL: &str = "select candidate_id, workspace_id,
    library_id, revision_id, chunk_id, candidate_label,
    candidate_type, candidate_sub_type, normalization_key, confidence, extraction_method,
    candidate_state, created_at, updated_at";
const RELATION_CANDIDATE_ROW_SQL: &str = "select candidate_id, workspace_id,
    library_id, revision_id, chunk_id, subject_label,
    subject_candidate_key, predicate, object_label, object_candidate_key, normalized_assertion,
    confidence, extraction_method, candidate_state, created_at, updated_at";
const ENTITY_ROW_SQL: &str = "select entity_id, workspace_id, library_id,
    \u{63}anonical_label, aliases, entity_type, entity_sub_type, summary, confidence,
    support_count, freshness_generation, entity_state, created_at, updated_at";
const RELATION_ROW_SQL: &str = "select relation_id, workspace_id, library_id,
    predicate, normalized_assertion, confidence, support_count, contradiction_state,
    freshness_generation, relation_state, created_at, updated_at";
const EVIDENCE_ROW_SQL: &str = "select evidence_id, workspace_id, library_id,
    document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end,
    quote_text, literal_spans_json, evidence_kind, extraction_method, confidence,
    evidence_state, freshness_generation, created_at, updated_at";

#[derive(Clone)]
pub struct PgGraphStore {
    pub pool: PgPool,
}

fn decode_json<T: DeserializeOwned>(value: serde_json::Value, context: &str) -> anyhow::Result<T> {
    serde_json::from_value(value).with_context(|| context.to_string())
}

fn decode_json_rows<T: DeserializeOwned>(
    values: Vec<serde_json::Value>,
    context: &str,
) -> anyhow::Result<Vec<T>> {
    values.into_iter().map(|value| decode_json(value, context)).collect()
}

fn clamp_limit(limit: usize) -> i64 {
    i64::try_from(limit.max(1)).unwrap_or(i64::MAX)
}

fn clamp_depth(max_depth: usize) -> i32 {
    i32::try_from(max_depth.clamp(1, 2)).unwrap_or(2)
}

mod candidates;
mod materialized;
mod projection;
mod traversal;

#[async_trait]
impl GraphStore for PgGraphStore {
    async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("select 1").execute(&self.pool).await.context("postgres graph ping failed")?;
        Ok(())
    }

    async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow> {
        self.upsert_entity_candidate_impl(input).await
    }

    async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.upsert_entity_candidates_impl(inputs).await
    }

    async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.list_entity_candidates_by_revision_impl(revision_id).await
    }

    async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.list_entity_candidates_by_library_impl(library_id).await
    }

    async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.delete_entity_candidates_by_revision_impl(revision_id).await
    }

    async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        self.delete_entity_candidates_by_library_impl(library_id).await
    }

    async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow> {
        self.upsert_relation_candidate_impl(input).await
    }

    async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.upsert_relation_candidates_impl(inputs).await
    }

    async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.list_relation_candidates_by_revision_impl(revision_id).await
    }

    async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.list_relation_candidates_by_library_impl(library_id).await
    }

    async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.delete_relation_candidates_by_revision_impl(revision_id).await
    }

    async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        self.delete_relation_candidates_by_library_impl(library_id).await
    }

    async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_document_revision_edge_impl(document_id, revision_id, library_id).await
    }

    async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_revision_chunk_edge_impl(revision_id, chunk_id, library_id).await
    }

    async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_revision_chunk_edges_impl(revision_id, chunk_ids, library_id).await
    }

    async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64> {
        self.delete_revision_chunk_edges_impl(revision_id).await
    }

    async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        self.upsert_chunk_mentions_entity_edge_impl(chunk_id, entity_id, ranking).await
    }

    async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_relation_subject_edge_impl(relation_id, subject_entity_id, library_id).await
    }

    async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_relation_object_edge_impl(relation_id, object_entity_id, library_id).await
    }

    async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_source_edge_impl(evidence_id, revision_id, library_id).await
    }

    async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_supports_entity_edge_impl(evidence_id, entity_id, ranking).await
    }

    async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        self.upsert_evidence_supports_relation_edge_impl(evidence_id, relation_id, ranking).await
    }

    async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.upsert_fact_supports_evidence_edge_impl(fact_id, evidence_id, library_id).await
    }

    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        self.replace_library_projection_impl(_library_id, _projection_version, _nodes, _edges).await
    }

    async fn refresh_library_projection_targets(
        &self,
        targets: LibraryProjectionTargets<'_>,
    ) -> Result<(), GraphViewWriteError> {
        self.refresh_library_projection_targets_impl(targets).await
    }

    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphViewData> {
        self.load_library_projection_impl(library_id, projection_version).await
    }

    async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        targets: EvidenceEdgeTargets,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        self.upsert_evidence_with_edges_impl(input, targets).await
    }

    async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        self.upsert_entity_impl(input).await
    }

    async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        self.upsert_entities_impl(inputs).await
    }

    async fn get_entity_by_id(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        self.get_entity_by_id_impl(entity_id).await
    }

    async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        primary_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        self.get_entity_by_library_and_label_impl(library_id, primary_label).await
    }

    async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        self.list_entities_by_library_impl(library_id).await
    }

    async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        self.upsert_relation_impl(input).await
    }

    async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        self.upsert_relation_with_endpoints_impl(
            input,
            subject_entity_id,
            object_entity_id,
            library_id,
        )
        .await
    }

    async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        self.upsert_relations_impl(inputs).await
    }

    async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        self.get_relation_by_id_impl(relation_id).await
    }

    async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        self.get_relation_by_library_and_assertion_impl(library_id, normalized_assertion).await
    }

    async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        self.list_relations_by_library_impl(library_id).await
    }

    async fn delete_entities_by_identity_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        self.delete_entities_by_identity_keys_impl(library_id, keys).await
    }

    async fn delete_relations_by_identity_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        self.delete_relations_by_identity_keys_impl(library_id, keys).await
    }

    async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        self.upsert_evidence_impl(input).await
    }

    async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        self.get_evidence_by_id_impl(evidence_id).await
    }

    async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_ids_impl(evidence_ids).await
    }

    async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_revision_impl(revision_id).await
    }

    async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        self.list_evidence_by_chunk_impl(chunk_id).await
    }

    async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        self.list_relation_topology_by_library_impl(library_id).await
    }

    async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        self.get_relation_topology_by_id_impl(relation_id).await
    }

    async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.list_entity_neighborhood_impl(entity_id, library_id, max_depth, limit).await
    }

    async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.expand_relation_centric_impl(relation_id, library_id, max_depth, limit).await
    }

    async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        self.list_relation_evidence_lookup_impl(relation_id, library_id, limit).await
    }
}
