use std::sync::Arc;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
        KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_DOCUMENT_REVISION_EDGE,
        KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION, KNOWLEDGE_ENTITY_COLLECTION,
        KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
        KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        KNOWLEDGE_GRAPH_NAME, KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
        KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_RELATION_OBJECT_EDGE,
        KNOWLEDGE_RELATION_SUBJECT_EDGE, KNOWLEDGE_REVISION_CHUNK_EDGE,
        KNOWLEDGE_REVISION_COLLECTION,
    },
    document_store::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow},
};

#[derive(Debug, Clone)]
pub struct GraphViewNodeWrite {
    pub node_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub aliases: Vec<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct GraphViewEdgeWrite {
    pub edge_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct GraphViewData {
    pub nodes: Vec<GraphViewNodeWrite>,
    pub edges: Vec<GraphViewEdgeWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphViewWriteError {
    #[error("graph write contention: {message}")]
    GraphWriteContention { message: String },
    #[error("graph persistence integrity: {message}")]
    GraphPersistenceIntegrity { message: String },
    #[error("graph write failure: {message}")]
    GraphWriteFailure { message: String },
}

impl GraphViewWriteError {
    #[must_use]
    pub const fn is_retryable_contention(&self) -> bool {
        matches!(self, Self::GraphWriteContention { .. })
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::GraphWriteContention { message }
            | Self::GraphPersistenceIntegrity { message }
            | Self::GraphWriteFailure { message } => message,
        }
    }
}

#[must_use]
pub fn sanitize_graph_view_writes(
    nodes: &[GraphViewNodeWrite],
    edges: &[GraphViewEdgeWrite],
) -> (Vec<GraphViewNodeWrite>, Vec<GraphViewEdgeWrite>, usize) {
    let mut ordered_nodes = nodes.to_vec();
    ordered_nodes.sort_by_key(|node| node.node_id);

    let available_node_ids =
        ordered_nodes.iter().map(|node| node.node_id).collect::<std::collections::BTreeSet<_>>();
    let mut ordered_edges = edges
        .iter()
        .filter(|edge| {
            available_node_ids.contains(&edge.from_node_id)
                && available_node_ids.contains(&edge.to_node_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    ordered_edges.sort_by_key(|edge| (edge.from_node_id, edge.to_node_id, edge.edge_id));

    let skipped_edge_count = edges.len().saturating_sub(ordered_edges.len());
    (ordered_nodes, ordered_edges, skipped_edge_count)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntityCandidateRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub candidate_label: String,
    pub candidate_type: String,
    pub normalization_key: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationCandidateRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub subject_candidate_key: String,
    pub predicate: String,
    pub object_candidate_key: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntityRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub aliases: Vec<String>,
    pub entity_type: String,
    pub summary: Option<String>,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub freshness_generation: i64,
    pub entity_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub contradiction_state: String,
    pub freshness_generation: i64,
    pub relation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEvidenceRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub evidence_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub excerpt: String,
    pub support_kind: String,
    pub extraction_method: String,
    pub confidence: Option<f64>,
    pub evidence_state: String,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGraphTraversalRow {
    pub path_length: i64,
    pub vertex_kind: String,
    pub vertex_id: Uuid,
    pub edge_kind: Option<String>,
    pub edge_key: Option<String>,
    pub edge_rank: Option<i32>,
    pub edge_score: Option<f64>,
    pub edge_inclusion_reason: Option<String>,
    pub vertex: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationEvidenceLookupRow {
    pub relation: KnowledgeRelationRow,
    pub evidence: KnowledgeEvidenceRow,
    pub support_edge_rank: Option<i32>,
    pub support_edge_score: Option<f64>,
    pub support_edge_inclusion_reason: Option<String>,
    pub source_document: Option<KnowledgeDocumentRow>,
    pub source_revision: Option<KnowledgeRevisionRow>,
    pub source_chunk: Option<KnowledgeChunkRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationTopologyRow {
    #[serde(flatten)]
    pub relation: KnowledgeRelationRow,
    pub subject_entity_id: Uuid,
    pub object_entity_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEntityCandidate {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub candidate_label: String,
    pub candidate_type: String,
    pub normalization_key: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeRelationCandidate {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub subject_candidate_key: String,
    pub predicate: String,
    pub object_candidate_key: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEntity {
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub aliases: Vec<String>,
    pub entity_type: String,
    pub summary: Option<String>,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub freshness_generation: i64,
    pub entity_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeRelation {
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub contradiction_state: String,
    pub freshness_generation: i64,
    pub relation_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEvidence {
    pub evidence_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub excerpt: String,
    pub support_kind: String,
    pub extraction_method: String,
    pub confidence: Option<f64>,
    pub evidence_state: String,
    pub freshness_generation: i64,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct ArangoGraphStore {
    client: Arc<ArangoClient>,
}

impl ArangoGraphStore {
    #[must_use]
    pub fn new(client: Arc<ArangoClient>) -> Self {
        Self { client }
    }

    #[must_use]
    pub fn client(&self) -> &Arc<ArangoClient> {
        &self.client
    }

    #[must_use]
    pub const fn backend_name(&self) -> &'static str {
        "arangodb"
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        self.client.ping().await
    }

    pub async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        let query = format!(
            "FOR relation IN {relation_collection}
             FILTER relation.library_id == @library_id
             LET subject = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {subject_edge}
                  FILTER entity.library_id == @library_id
                  LIMIT 1
                  RETURN entity
             )
             LET object = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {object_edge}
                  FILTER entity.library_id == @library_id
                  LIMIT 1
                  RETURN entity
             )
             FILTER subject != null AND object != null
             SORT relation.support_count DESC, relation.updated_at DESC, relation.relation_id DESC
             RETURN MERGE(
                relation,
                {{
                  subject_entity_id: subject.entity_id,
                  object_entity_id: object.entity_id
                }}
             )",
            relation_collection = KNOWLEDGE_RELATION_COLLECTION,
            subject_edge = KNOWLEDGE_RELATION_SUBJECT_EDGE,
            object_edge = KNOWLEDGE_RELATION_OBJECT_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relation topology by library")?;
        decode_many_results(cursor)
    }

    pub async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        let query = format!(
            "FOR relation IN {relation_collection}
             FILTER relation.relation_id == @relation_id
             LET subject = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {subject_edge}
                  LIMIT 1
                  RETURN entity
             )
             LET object = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {object_edge}
                  LIMIT 1
                  RETURN entity
             )
             FILTER subject != null AND object != null
             LIMIT 1
             RETURN MERGE(
                relation,
                {{
                  subject_entity_id: subject.entity_id,
                  object_entity_id: object.entity_id
                }}
             )",
            relation_collection = KNOWLEDGE_RELATION_COLLECTION,
            subject_edge = KNOWLEDGE_RELATION_SUBJECT_EDGE,
            object_edge = KNOWLEDGE_RELATION_OBJECT_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "relation_id": relation_id,
                }),
            )
            .await
            .context("failed to get knowledge relation topology by id")?;
        decode_optional_single_result(cursor)
    }

    pub async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    candidate_id: @candidate_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    candidate_label: @candidate_label,
                    candidate_type: @candidate_type,
                    normalization_key: @normalization_key,
                    confidence: @confidence,
                    extraction_method: @extraction_method,
                    candidate_state: @candidate_state,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    candidate_label: @candidate_label,
                    candidate_type: @candidate_type,
                    normalization_key: @normalization_key,
                    confidence: @confidence,
                    extraction_method: @extraction_method,
                    candidate_state: @candidate_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "key": input.candidate_id,
                    "candidate_id": input.candidate_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "candidate_label": input.candidate_label,
                    "candidate_type": input.candidate_type,
                    "normalization_key": input.normalization_key,
                    "confidence": input.confidence,
                    "extraction_method": input.extraction_method,
                    "candidate_state": input.candidate_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge entity candidate")?;
        decode_single_result(cursor)
    }

    pub async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let relation = self.upsert_relation(input).await?;
        if let Some(subject_entity_id) = subject_entity_id {
            self.upsert_relation_subject_edge(relation.relation_id, subject_entity_id).await?;
        }
        if let Some(object_entity_id) = object_entity_id {
            self.upsert_relation_object_edge(relation.relation_id, object_entity_id).await?;
        }
        Ok(relation)
    }

    pub async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge entity candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge entity candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete knowledge entity candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to delete knowledge entity candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    candidate_id: @candidate_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    subject_candidate_key: @subject_candidate_key,
                    predicate: @predicate,
                    object_candidate_key: @object_candidate_key,
                    normalized_assertion: @normalized_assertion,
                    confidence: @confidence,
                    extraction_method: @extraction_method,
                    candidate_state: @candidate_state,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    subject_candidate_key: @subject_candidate_key,
                    predicate: @predicate,
                    object_candidate_key: @object_candidate_key,
                    normalized_assertion: @normalized_assertion,
                    confidence: @confidence,
                    extraction_method: @extraction_method,
                    candidate_state: @candidate_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "key": input.candidate_id,
                    "candidate_id": input.candidate_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "subject_candidate_key": input.subject_candidate_key,
                    "predicate": input.predicate,
                    "object_candidate_key": input.object_candidate_key,
                    "normalized_assertion": input.normalized_assertion,
                    "confidence": input.confidence,
                    "extraction_method": input.extraction_method,
                    "candidate_state": input.candidate_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge relation candidate")?;
        decode_single_result(cursor)
    }

    pub async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let evidence = self.upsert_evidence(input).await?;
        if let Some(source_revision_id) = source_revision_id {
            self.upsert_evidence_source_edge(evidence.evidence_id, source_revision_id).await?;
        }
        if let Some(supporting_entity_id) = supporting_entity_id {
            self.upsert_evidence_supports_entity_edge(
                evidence.evidence_id,
                supporting_entity_id,
                None,
                None,
                None,
            )
            .await?;
        }
        if let Some(supporting_relation_id) = supporting_relation_id {
            self.upsert_evidence_supports_relation_edge(
                evidence.evidence_id,
                supporting_relation_id,
                None,
                None,
                None,
            )
            .await?;
        }
        Ok(evidence)
    }

    pub async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge relation candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relation candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete knowledge relation candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to delete knowledge relation candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    entity_id: @entity_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    canonical_label: @canonical_label,
                    aliases: @aliases,
                    entity_type: @entity_type,
                    summary: @summary,
                    confidence: @confidence,
                    support_count: @support_count,
                    freshness_generation: @freshness_generation,
                    entity_state: @entity_state,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    canonical_label: @canonical_label,
                    aliases: UNION_DISTINCT(COALESCE(OLD.aliases, []), @aliases),
                    entity_type: @entity_type,
                    summary: @summary,
                    confidence: @confidence,
                    support_count: @support_count,
                    freshness_generation: @freshness_generation,
                    entity_state: @entity_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "key": input.entity_id,
                    "entity_id": input.entity_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "canonical_label": input.canonical_label,
                    "aliases": input.aliases,
                    "entity_type": input.entity_type,
                    "summary": input.summary,
                    "confidence": input.confidence,
                    "support_count": input.support_count,
                    "freshness_generation": input.freshness_generation,
                    "entity_state": input.entity_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge entity")?;
        decode_single_result(cursor)
    }

    pub async fn get_entity_by_id(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.entity_id == @entity_id
                 LIMIT 1
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "entity_id": entity_id,
                }),
            )
            .await
            .context("failed to get knowledge entity")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        canonical_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.library_id == @library_id
                   AND entity.canonical_label == @canonical_label
                 LIMIT 1
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "library_id": library_id,
                    "canonical_label": canonical_label,
                }),
            )
            .await
            .context("failed to lookup knowledge entity by label")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.library_id == @library_id
                 SORT entity.support_count DESC, entity.updated_at DESC, entity.entity_id DESC
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge entities by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    relation_id: @relation_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    predicate: @predicate,
                    normalized_assertion: @normalized_assertion,
                    confidence: @confidence,
                    support_count: @support_count,
                    contradiction_state: @contradiction_state,
                    freshness_generation: @freshness_generation,
                    relation_state: @relation_state,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    predicate: @predicate,
                    normalized_assertion: @normalized_assertion,
                    confidence: @confidence,
                    support_count: @support_count,
                    contradiction_state: @contradiction_state,
                    freshness_generation: @freshness_generation,
                    relation_state: @relation_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "key": input.relation_id,
                    "relation_id": input.relation_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "predicate": input.predicate,
                    "normalized_assertion": input.normalized_assertion,
                    "confidence": input.confidence,
                    "support_count": input.support_count,
                    "contradiction_state": input.contradiction_state,
                    "freshness_generation": input.freshness_generation,
                    "relation_state": input.relation_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge relation")?;
        decode_single_result(cursor)
    }

    pub async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.relation_id == @relation_id
                 LIMIT 1
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "relation_id": relation_id,
                }),
            )
            .await
            .context("failed to get knowledge relation")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.library_id == @library_id
                   AND relation.normalized_assertion == @normalized_assertion
                 LIMIT 1
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "library_id": library_id,
                    "normalized_assertion": normalized_assertion,
                }),
            )
            .await
            .context("failed to lookup knowledge relation by assertion")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.library_id == @library_id
                 SORT relation.support_count DESC, relation.updated_at DESC, relation.relation_id DESC
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relations by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    evidence_id: @evidence_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    excerpt: @excerpt,
                    support_kind: @support_kind,
                    extraction_method: @extraction_method,
                    confidence: @confidence,
                    evidence_state: @evidence_state,
                    freshness_generation: @freshness_generation,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    excerpt: @excerpt,
                    support_kind: @support_kind,
                    extraction_method: @extraction_method,
                    confidence: @confidence,
                    evidence_state: @evidence_state,
                    freshness_generation: @freshness_generation,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "key": input.evidence_id,
                    "evidence_id": input.evidence_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "document_id": input.document_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "span_start": input.span_start,
                    "span_end": input.span_end,
                    "excerpt": input.excerpt,
                    "support_kind": input.support_kind,
                    "extraction_method": input.extraction_method,
                    "confidence": input.confidence,
                    "evidence_state": input.evidence_state,
                    "freshness_generation": input.freshness_generation,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge evidence")?;
        decode_single_result(cursor)
    }

    pub async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.evidence_id == @evidence_id
                 LIMIT 1
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "evidence_id": evidence_id,
                }),
            )
            .await
            .context("failed to get knowledge evidence")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.revision_id == @revision_id
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge evidence by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.chunk_id == @chunk_id
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .context("failed to list knowledge evidence by chunk")?;
        decode_many_results(cursor)
    }

    pub async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vertex, edge, path IN 0..@max_depth ANY @start_vertex GRAPH @graph_name
                 OPTIONS { bfs: true, uniqueVertices: \"global\" }
                 FILTER HAS(vertex, \"library_id\")
                   AND vertex.library_id == @library_id
                 LET vertex_kind = COLLECTION_NAME(vertex)
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex.document_id
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : COLLECTION_NAME(edge),
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                 }",
                serde_json::json!({
                    "@graph_name": KNOWLEDGE_GRAPH_NAME,
                    "@start_vertex": format!("{}/{}", KNOWLEDGE_ENTITY_COLLECTION, entity_id),
                    "@library_id": library_id,
                    "@max_depth": max_depth.max(1),
                    "@limit": limit.max(1),
                    "@entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "@evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "@revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                }),
            )
            .await
            .context("failed to list knowledge entity neighborhood")?;
        decode_many_results(cursor)
    }

    pub async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vertex, edge, path IN 0..@max_depth ANY @start_vertex GRAPH @graph_name
                 OPTIONS { bfs: true, uniqueVertices: \"global\" }
                 FILTER HAS(vertex, \"library_id\")
                   AND vertex.library_id == @library_id
                 LET vertex_kind = COLLECTION_NAME(vertex)
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex.document_id
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : COLLECTION_NAME(edge),
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                 }",
                serde_json::json!({
                    "@graph_name": KNOWLEDGE_GRAPH_NAME,
                    "@start_vertex": format!("{}/{}", KNOWLEDGE_RELATION_COLLECTION, relation_id),
                    "@library_id": library_id,
                    "@max_depth": max_depth.max(1),
                    "@limit": limit.max(1),
                    "@entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "@evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "@revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                }),
            )
            .await
            .context("failed to expand knowledge relation-centric neighborhood")?;
        decode_many_results(cursor)
    }

    pub async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@relation_collection
                 FILTER relation.relation_id == @relation_id
                   AND relation.library_id == @library_id
                 FOR evidence, edge, path IN 1..1 INBOUND CONCAT(@relation_collection, \"/\", relation.relation_id) GRAPH @graph_name
                 FILTER COLLECTION_NAME(evidence) == @evidence_collection
                 SORT edge.rank ASC, edge.created_at ASC, evidence.created_at ASC, evidence.evidence_id ASC
                 LIMIT @limit
                 LET source_document = FIRST(
                    FOR document IN @@document_collection
                      FILTER document.document_id == evidence.document_id
                      LIMIT 1
                      RETURN document
                 )
                 LET source_revision = FIRST(
                    FOR revision IN @@revision_collection
                      FILTER revision.revision_id == evidence.revision_id
                      LIMIT 1
                      RETURN revision
                 )
                 LET source_chunk = FIRST(
                    FOR chunk IN @@chunk_collection
                      FILTER evidence.chunk_id != null
                        AND chunk.chunk_id == evidence.chunk_id
                      LIMIT 1
                      RETURN chunk
                 )
                 RETURN {
                    relation,
                    evidence,
                    support_edge_rank: edge.rank,
                    support_edge_score: edge.score,
                    support_edge_inclusion_reason: edge.inclusionReason,
                    source_document,
                    source_revision,
                    source_chunk
                 }",
                serde_json::json!({
                    "@graph_name": KNOWLEDGE_GRAPH_NAME,
                    "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "@evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "@document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "@revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "relation_id": relation_id,
                    "library_id": library_id,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to lookup evidence-backed knowledge relation")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            KNOWLEDGE_DOCUMENT_COLLECTION,
            document_id,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_REVISION_CHUNK_EDGE,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            KNOWLEDGE_CHUNK_COLLECTION,
            chunk_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        self.client
            .query_json(
                "FOR chunk_id IN @chunk_ids
                 INSERT {
                    _from: @from_id,
                    _to: CONCAT(@to_collection, '/', chunk_id),
                    created_at: @created_at
                 } INTO @@collection
                 RETURN NEW._id",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_CHUNK_EDGE,
                    "from_id": format!("{}/{}", KNOWLEDGE_REVISION_COLLECTION, revision_id),
                    "to_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "chunk_ids": chunk_ids,
                    "created_at": Utc::now(),
                }),
            )
            .await
            .with_context(|| "failed to insert revision chunk edges".to_string())?;
        Ok(())
    }

    pub async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64> {
        let cursor = self
            .client
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge._from == @from_id
                 REMOVE edge IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_CHUNK_EDGE,
                    "from_id": format!("{}/{}", KNOWLEDGE_REVISION_COLLECTION, revision_id),
                }),
            )
            .await
            .context("failed to delete revision chunk edges")?;
        let removed: Vec<serde_json::Value> = decode_many_results(cursor)?;
        Ok(u64::try_from(removed.len()).unwrap_or(u64::MAX))
    }

    pub async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            KNOWLEDGE_CHUNK_COLLECTION,
            chunk_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            entity_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    pub async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_RELATION_SUBJECT_EDGE,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            subject_entity_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_RELATION_OBJECT_EDGE,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            object_entity_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            entity_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    pub async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    async fn insert_edge(
        &self,
        collection: &str,
        from_collection: &str,
        from_id: Uuid,
        to_collection: &str,
        to_id: Uuid,
        extra_fields: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut payload = serde_json::json!({
            "@collection": collection,
            "_from": format!("{}/{}", from_collection, from_id),
            "_to": format!("{}/{}", to_collection, to_id),
            "created_at": Utc::now(),
        });
        if let (Some(target), Some(source)) = (payload.as_object_mut(), extra_fields.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        } else {
            return Err(anyhow!("failed to build edge payload"));
        }

        self.client
            .query_json(
                "INSERT @payload INTO @@collection RETURN NEW",
                serde_json::json!({
                    "@collection": collection,
                    "payload": payload,
                }),
            )
            .await
            .with_context(|| format!("failed to insert edge into {collection}"))?;
        Ok(())
    }

    pub async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    pub async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    pub async fn load_library_projection(
        &self,
        library_id: Uuid,
        _projection_version: i64,
    ) -> anyhow::Result<GraphViewData> {
        let nodes = self
            .list_entities_by_library(library_id)
            .await?
            .into_iter()
            .map(|entity| GraphViewNodeWrite {
                node_id: entity.entity_id,
                canonical_key: entity.key,
                label: entity.canonical_label,
                node_type: entity.entity_type,
                support_count: i32::try_from(entity.support_count).unwrap_or(i32::MAX),
                summary: entity.summary,
                aliases: entity.aliases,
                metadata_json: serde_json::json!({
                    "entity_state": entity.entity_state,
                    "freshness_generation": entity.freshness_generation,
                    "confidence": entity.confidence,
                }),
            })
            .collect::<Vec<_>>();
        let edges = self
            .list_relation_topology_by_library(library_id)
            .await?
            .into_iter()
            .map(|row| GraphViewEdgeWrite {
                edge_id: row.relation.relation_id,
                from_node_id: row.subject_entity_id,
                to_node_id: row.object_entity_id,
                relation_type: row.relation.predicate,
                canonical_key: row.relation.normalized_assertion,
                support_count: i32::try_from(row.relation.support_count).unwrap_or(i32::MAX),
                summary: None,
                weight: row.relation.confidence,
                metadata_json: serde_json::json!({
                    "relation_state": row.relation.relation_state,
                    "freshness_generation": row.relation.freshness_generation,
                    "contradiction_state": row.relation.contradiction_state,
                }),
            })
            .collect::<Vec<_>>();
        Ok(GraphViewData { nodes, edges })
    }
}

fn decode_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    decode_optional_single_result(cursor)?.ok_or_else(|| anyhow!("ArangoDB query returned no rows"))
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    let mut rows: Vec<T> =
        serde_json::from_value(result).context("failed to decode ArangoDB result rows")?;
    Ok((!rows.is_empty()).then(|| rows.remove(0)))
}

fn decode_many_results<T>(cursor: serde_json::Value) -> anyhow::Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}
