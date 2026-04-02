use std::{sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::time::sleep;
use uuid::Uuid;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        KNOWLEDGE_BUNDLE_RELATION_EDGE, KNOWLEDGE_CHUNK_COLLECTION,
        KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE, KNOWLEDGE_DOCUMENT_COLLECTION,
        KNOWLEDGE_DOCUMENT_REVISION_EDGE, KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
        KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
        KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        KNOWLEDGE_FACT_EVIDENCE_EDGE, KNOWLEDGE_GRAPH_NAME,
        KNOWLEDGE_RELATION_CANDIDATE_COLLECTION, KNOWLEDGE_RELATION_COLLECTION,
        KNOWLEDGE_RELATION_OBJECT_EDGE, KNOWLEDGE_RELATION_SUBJECT_EDGE,
        KNOWLEDGE_REVISION_CHUNK_EDGE, KNOWLEDGE_REVISION_COLLECTION,
        KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
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
    #[serde(default)]
    pub subject_label: String,
    pub subject_candidate_key: String,
    pub predicate: String,
    #[serde(default)]
    pub object_label: String,
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
    #[serde(default)]
    pub chunk_id: Option<Uuid>,
    #[serde(default)]
    pub block_id: Option<Uuid>,
    #[serde(default)]
    pub fact_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    #[serde(default)]
    pub quote_text: String,
    #[serde(default)]
    pub literal_spans_json: serde_json::Value,
    #[serde(default)]
    pub evidence_kind: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocumentGraphLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
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
    pub subject_label: String,
    pub subject_candidate_key: String,
    pub predicate: String,
    pub object_label: String,
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
    pub block_id: Option<Uuid>,
    pub fact_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub quote_text: String,
    pub literal_spans_json: serde_json::Value,
    pub evidence_kind: String,
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
    const LIBRARY_RESET_BATCH_SIZE: usize = 512;

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
             LIMIT 10000
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

    pub async fn list_document_graph_links_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeDocumentGraphLinkRow>> {
        let query = format!(
            "FOR document IN {document_collection}
               FILTER document.library_id == @library_id
                 AND document.deleted_at == null
               LET revision_id = document.active_revision_id != null
                 ? document.active_revision_id
                 : document.readable_revision_id
               FILTER revision_id != null
               LET revision_vertex_id = CONCAT(@revision_collection, '/', revision_id)
               LET mention_rows = (
                 FOR chunk IN OUTBOUND revision_vertex_id {revision_chunk_edge_collection}
                   FILTER chunk.library_id == @library_id
                   FOR entity, edge IN OUTBOUND chunk._id {mention_edge_collection}
                     FILTER entity != null
                       AND entity.library_id == @library_id
                     COLLECT target_node_id = entity.entity_id
                     AGGREGATE mention_count = COUNT(1)
                     RETURN {{
                        document_id: document.document_id,
                        target_node_id,
                        target_node_type: \"entity\",
                        mention_count,
                        support_count: 0
                     }}
               )
               LET evidence_rows = (
                 FOR evidence IN INBOUND revision_vertex_id {evidence_source_edge_collection}
                   FILTER evidence != null
                     AND evidence.library_id == @library_id
                   LET entity_rows = (
                     FOR entity, edge IN OUTBOUND evidence._id {evidence_support_entity_edge_collection}
                       FILTER entity != null
                         AND entity.library_id == @library_id
                       COLLECT target_node_id = entity.entity_id
                       AGGREGATE support_count = COUNT(1)
                       RETURN {{
                          document_id: document.document_id,
                          target_node_id,
                          target_node_type: \"entity\",
                          mention_count: 0,
                          support_count
                       }}
                   )
                   LET relation_rows = (
                     FOR relation, edge IN OUTBOUND evidence._id {evidence_support_relation_edge_collection}
                       FILTER relation != null
                         AND relation.library_id == @library_id
                       COLLECT target_node_id = relation.relation_id
                       AGGREGATE support_count = COUNT(1)
                       RETURN {{
                          document_id: document.document_id,
                          target_node_id,
                          target_node_type: \"topic\",
                          mention_count: 0,
                          support_count
                       }}
                   )
                   RETURN UNION(entity_rows, relation_rows)
               )
               LET rows = APPEND(mention_rows, FLATTEN(evidence_rows))
               FOR row IN rows
                 COLLECT
                   document_id = row.document_id,
                   target_node_id = row.target_node_id,
                   target_node_type = row.target_node_type
                 AGGREGATE
                   mention_count = SUM(row.mention_count),
                   support_count = SUM(row.support_count)
                 LET total_support_count = mention_count + support_count
                 FILTER total_support_count > 0
                 LET relation_type =
                   target_node_type == \"entity\" && mention_count > 0 ? \"mentions\" : \"supports\"
                 SORT total_support_count DESC, document_id ASC, target_node_type ASC, target_node_id ASC
                 RETURN {{
                    document_id,
                    target_node_id,
                    target_node_type,
                    relation_type,
                    support_count: total_support_count
                 }}",
            document_collection = KNOWLEDGE_DOCUMENT_COLLECTION,
            revision_chunk_edge_collection = KNOWLEDGE_REVISION_CHUNK_EDGE,
            evidence_source_edge_collection = KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            mention_edge_collection = KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            evidence_support_entity_edge_collection = KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            evidence_support_relation_edge_collection = KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "library_id": library_id,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                }),
            )
            .await
            .context("failed to list knowledge document graph links")?;
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
        let mut rows = self.upsert_entity_candidates(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no entity candidate rows"))
    }

    pub async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
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
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    candidate_id: row.candidate_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    candidate_label: row.candidate_label,
                    candidate_type: row.candidate_type,
                    normalization_key: row.normalization_key,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    candidate_label: row.candidate_label,
                    candidate_type: row.candidate_type,
                    normalization_key: row.normalization_key,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge entity candidates",
            )
            .await?;
        decode_many_results(cursor)
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
        let mut rows = self.upsert_relation_candidates(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no relation candidate rows"))
    }

    pub async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
                    "key": input.candidate_id,
                    "candidate_id": input.candidate_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "subject_label": input.subject_label,
                    "subject_candidate_key": input.subject_candidate_key,
                    "predicate": input.predicate,
                    "object_label": input.object_label,
                    "object_candidate_key": input.object_candidate_key,
                    "normalized_assertion": input.normalized_assertion,
                    "confidence": input.confidence,
                    "extraction_method": input.extraction_method,
                    "candidate_state": input.candidate_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    candidate_id: row.candidate_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    subject_label: row.subject_label,
                    subject_candidate_key: row.subject_candidate_key,
                    predicate: row.predicate,
                    object_label: row.object_label,
                    object_candidate_key: row.object_candidate_key,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    subject_label: row.subject_label,
                    subject_candidate_key: row.subject_candidate_key,
                    predicate: row.predicate,
                    object_label: row.object_label,
                    object_candidate_key: row.object_candidate_key,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge relation candidates",
            )
            .await?;
        decode_many_results(cursor)
    }

    pub async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
        supporting_fact_id: Option<Uuid>,
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
        if let Some(supporting_fact_id) = supporting_fact_id {
            self.upsert_fact_supports_evidence_edge(supporting_fact_id, evidence.evidence_id)
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

    pub async fn reset_library_materialized_graph(&self, library_id: Uuid) -> anyhow::Result<()> {
        self.delete_edges_by_library_reference(
            KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            "_to",
            library_id,
            "failed to delete document-revision edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_REVISION_CHUNK_EDGE,
            "_from",
            library_id,
            "failed to delete revision-chunk edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            "_to",
            library_id,
            "failed to delete chunk mention edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_RELATION_SUBJECT_EDGE,
            "_from",
            library_id,
            "failed to delete relation subject edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_RELATION_OBJECT_EDGE,
            "_from",
            library_id,
            "failed to delete relation object edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            "_from",
            library_id,
            "failed to delete evidence source edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            "_from",
            library_id,
            "failed to delete evidence-entity edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
            "_from",
            library_id,
            "failed to delete evidence-relation edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_FACT_EVIDENCE_EDGE,
            "_to",
            library_id,
            "failed to delete fact-evidence edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_ENTITY_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-entity edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_RELATION_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-relation edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-evidence edges for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_EVIDENCE_COLLECTION,
            library_id,
            "failed to delete evidence rows for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_RELATION_COLLECTION,
            library_id,
            "failed to delete relation rows for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_ENTITY_COLLECTION,
            library_id,
            "failed to delete entity rows for library graph reset",
        )
        .await?;
        Ok(())
    }

    pub async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        let mut rows = self.upsert_entities(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no entity rows"))
    }

    pub async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
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
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    entity_id: row.entity_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    canonical_label: row.canonical_label,
                    aliases: row.aliases,
                    entity_type: row.entity_type,
                    summary: row.summary,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    freshness_generation: row.freshness_generation,
                    entity_state: row.entity_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    canonical_label: row.canonical_label,
                    aliases: UNION_DISTINCT((OLD.aliases == null ? [] : OLD.aliases), row.aliases),
                    entity_type: row.entity_type,
                    summary: row.summary,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    freshness_generation: row.freshness_generation,
                    entity_state: row.entity_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge entities",
            )
            .await?;
        decode_many_results(cursor)
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
                 LIMIT 5000
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
        let mut rows = self.upsert_relations(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no relation rows"))
    }

    pub async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
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
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    relation_id: row.relation_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    predicate: row.predicate,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    contradiction_state: row.contradiction_state,
                    freshness_generation: row.freshness_generation,
                    relation_state: row.relation_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    predicate: row.predicate,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    contradiction_state: row.contradiction_state,
                    freshness_generation: row.freshness_generation,
                    relation_state: row.relation_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge relations",
            )
            .await?;
        decode_many_results(cursor)
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
                    block_id: @block_id,
                    fact_id: @fact_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    quote_text: @quote_text,
                    literal_spans_json: @literal_spans_json,
                    evidence_kind: @evidence_kind,
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
                    block_id: @block_id,
                    fact_id: @fact_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    quote_text: @quote_text,
                    literal_spans_json: @literal_spans_json,
                    evidence_kind: @evidence_kind,
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
                    "block_id": input.block_id,
                    "fact_id": input.fact_id,
                    "span_start": input.span_start,
                    "span_end": input.span_end,
                    "quote_text": input.quote_text,
                    "literal_spans_json": input.literal_spans_json,
                    "evidence_kind": input.evidence_kind,
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

    pub async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        if evidence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.evidence_id IN @evidence_ids
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "evidence_ids": evidence_ids,
                }),
            )
            .await
            .context("failed to list knowledge evidence by ids")?;
        decode_many_results(cursor)
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
                 LET vertex_kind = PARSE_IDENTIFIER(vertex._id).collection
                 FILTER vertex_kind == @entity_collection
                    OR vertex_kind == @relation_collection
                    OR vertex_kind == @evidence_collection
                    OR vertex_kind == @chunk_collection
                    OR vertex_kind == @revision_collection
                    OR vertex_kind == @document_collection
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex_kind == @document_collection ? vertex.document_id :
                     null
                 FILTER vertex_id != null
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : PARSE_IDENTIFIER(edge._id).collection,
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                }",
                serde_json::json!({
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "start_vertex": format!("{}/{}", KNOWLEDGE_ENTITY_COLLECTION, entity_id),
                    "library_id": library_id,
                    "max_depth": max_depth.max(1),
                    "limit": limit.max(1),
                    "entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
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
                 LET vertex_kind = PARSE_IDENTIFIER(vertex._id).collection
                 FILTER vertex_kind == @entity_collection
                    OR vertex_kind == @relation_collection
                    OR vertex_kind == @evidence_collection
                    OR vertex_kind == @chunk_collection
                    OR vertex_kind == @revision_collection
                    OR vertex_kind == @document_collection
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex_kind == @document_collection ? vertex.document_id :
                     null
                 FILTER vertex_id != null
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : PARSE_IDENTIFIER(edge._id).collection,
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                }",
                serde_json::json!({
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "start_vertex": format!("{}/{}", KNOWLEDGE_RELATION_COLLECTION, relation_id),
                    "library_id": library_id,
                    "max_depth": max_depth.max(1),
                    "limit": limit.max(1),
                    "entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
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
                 FOR evidence, edge, path IN 1..1 INBOUND relation._id GRAPH @graph_name
                 FILTER PARSE_IDENTIFIER(evidence._id).collection == @evidence_collection
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
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
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
        for chunk_id in chunk_ids {
            self.upsert_revision_chunk_edge(revision_id, *chunk_id).await?;
        }
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

    pub async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_FACT_EVIDENCE_EDGE,
            KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
            fact_id,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            serde_json::json!({}),
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
            "_key": canonical_edge_key(from_id, to_id),
            "@collection": collection,
            "_from": format!("{}/{}", from_collection, from_id),
            "_to": format!("{}/{}", to_collection, to_id),
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
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
                "UPSERT { _key: @payload._key }
                 INSERT @payload
                 UPDATE MERGE(@payload, { created_at: OLD.created_at })
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": collection,
                    "payload": payload,
                }),
            )
            .await
            .with_context(|| format!("failed to insert edge into {collection}"))?;
        Ok(())
    }

    async fn delete_collection_documents_by_library(
        &self,
        collection: &str,
        library_id: Uuid,
        error_context: &str,
    ) -> anyhow::Result<()> {
        loop {
            let cursor = self
                .client
                .query_json(
                    "FOR doc IN @@collection
                     FILTER doc.library_id == @library_id
                     LIMIT @limit
                     RETURN doc._key",
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                        "limit": Self::LIBRARY_RESET_BATCH_SIZE,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
            let keys: Vec<String> = decode_many_results(cursor)?;
            if keys.is_empty() {
                break;
            }

            self.client
                .query_json(
                    "FOR key IN @keys
                     REMOVE { _key: key } IN @@collection
                     OPTIONS { ignoreErrors: true }",
                    serde_json::json!({
                        "@collection": collection,
                        "keys": keys,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
        }
        Ok(())
    }

    async fn delete_edges_by_library_reference(
        &self,
        collection: &str,
        vertex_field: &str,
        library_id: Uuid,
        error_context: &str,
    ) -> anyhow::Result<()> {
        loop {
            let query = format!(
                "FOR edge IN @@collection
                 LET vertex = DOCUMENT(edge.{vertex_field})
                 FILTER vertex != null
                   AND vertex.library_id == @library_id
                 LIMIT @limit
                 RETURN edge._key"
            );
            let cursor = self
                .client
                .query_json(
                    &query,
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                        "limit": Self::LIBRARY_RESET_BATCH_SIZE,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
            let keys: Vec<String> = decode_many_results(cursor)?;
            if keys.is_empty() {
                break;
            }

            self.client
                .query_json(
                    "FOR key IN @keys
                     REMOVE { _key: key } IN @@collection
                     OPTIONS { ignoreErrors: true }",
                    serde_json::json!({
                        "@collection": collection,
                        "keys": keys,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
        }
        Ok(())
    }

    async fn run_retryable_upsert_query(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        context_message: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let mut last_error = None;
        for attempt in 0..3 {
            match self.client.query_json(query, bind_vars.clone()).await {
                Ok(cursor) => return Ok(cursor),
                Err(error) => {
                    let message = format!("{error:#}");
                    if attempt < 2 && is_retryable_upsert_error(&message) {
                        sleep(Duration::from_millis(100 * (1 << attempt))).await;
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error).context(context_message.to_string());
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("retryable ArangoDB upsert failed")))
            .context(context_message.to_string())
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

fn canonical_edge_key(from_id: Uuid, to_id: Uuid) -> String {
    format!("{from_id}:{to_id}")
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

fn is_retryable_upsert_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("write-write conflict")
        || normalized.contains("operation timed out")
        || normalized.contains("timed out")
        || normalized.contains("timeout")
        || normalized.contains("status 500")
        || normalized.contains("status 503")
}
