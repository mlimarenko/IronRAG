#![allow(
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::unused_async,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

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

mod candidates;
mod edges_or_projection;
mod materialized;
mod traversal;

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
    #[serde(default)]
    pub candidate_sub_type: Option<String>,
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
    #[serde(default)]
    pub entity_sub_type: Option<String>,
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
    pub candidate_sub_type: Option<String>,
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
    pub entity_sub_type: Option<String>,
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

    pub async fn ping(&self) -> anyhow::Result<()> {
        self.client.ping().await
    }

    #[must_use]
    pub const fn backend_name(&self) -> &'static str {
        "arangodb"
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
