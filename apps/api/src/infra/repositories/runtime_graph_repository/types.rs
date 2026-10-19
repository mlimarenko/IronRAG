use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphFilteredArtifactRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub ingestion_run_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub target_kind: String,
    pub candidate_key: String,
    pub source_node_key: Option<String>,
    pub target_node_key: Option<String>,
    pub relation_type: Option<String>,
    pub filter_reason: String,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphNodeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    #[serde(rename = "\u{63}anonical_key")]
    #[sqlx(rename = "\u{63}anonical_key")]
    pub identity_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphEdgeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    #[serde(rename = "\u{63}anonical_key")]
    #[sqlx(rename = "\u{63}anonical_key")]
    pub identity_key: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub metadata_json: serde_json::Value,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lean projection of a graph node for adjacency-only consumers (community
/// detection / label propagation). Holds only the normalized id — no heavy
/// text columns (`summary`, `label`, `identity_key`, JSON blobs) — so the
/// in-RAM footprint per row stays at 16 bytes instead of multi-KB.
///
/// See `list_runtime_graph_node_ids_by_library`.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct RuntimeGraphNodeIdRow {
    pub id: Uuid,
}

/// Lean projection of a graph edge for adjacency-only consumers. Holds only
/// the columns label-propagation actually reads (`from`, `to`,
/// `support_count`) so large libraries avoid materializing heavy edge payloads
/// in memory.
///
/// See `list_runtime_graph_edges_adjacency_by_library`.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct RuntimeGraphEdgeAdjacencyRow {
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub support_count: i32,
}

/// Slim node row for the query-time graph index. Drops `metadata_json`
/// (the largest per-row allocation — a `serde_json::Value` heap object
/// that query-time callers never read) while keeping every other column
/// that `graph_retrieval.rs`, `retrieve.rs`, and `context.rs` actually
/// access: `id`, `library_id`, `identity_key`, `label`, `node_type`,
/// `aliases_json`, `summary`, `support_count`, `projection_version`,
/// `created_at`, `updated_at`.
///
/// On a large corpus this drops one `serde_json::Value` heap allocation per
/// node on every cache-miss load. Per-row savings depend on the average
/// `metadata_json` payload size; typical `sub_type` objects are 50–300 bytes,
/// so at six-figure node counts this saves on the order of tens of MB for
/// nodes alone.
///
/// See `list_runtime_graph_query_nodes_by_ids_or_document_type`.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphQueryNodeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    #[sqlx(rename = "\u{63}anonical_key")]
    pub identity_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Slim edge row for the query-time graph index. Drops `metadata_json`
/// and `identity_key` — neither is accessed by any query-time caller
/// (`graph_retrieval.rs`, `retrieve.rs`, `context.rs`). The remaining
/// columns cover all accesses: `id`, `library_id`, `from_node_id`,
/// `to_node_id`, `relation_type`, `summary`, `weight`, `support_count`,
/// `projection_version`, `created_at`, `updated_at`.
///
/// On a large corpus this drops one `serde_json::Value` heap allocation plus
/// the `identity_key` String per edge. Savings scale with edge count and
/// `metadata_json` population density — on the order of tens to a couple
/// hundred MB at six-figure edge counts.
///
/// See `list_admitted_runtime_graph_query_edges_by_library`.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphQueryEdgeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphEvidenceRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphEvidenceTargetRow {
    pub target_kind: String,
    pub target_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphEvidenceLifecycleRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// A graph-evidence query with explicit lane provenance.
///
/// Natural-language questions stay in the full-text lexical lane. Only values
/// emitted by the typed query compiler (or by a formal-syntax parser) may use
/// the literal `LIKE` lane. The repository deliberately does not infer this
/// distinction from casing, vocabulary, or other raw-text shape heuristics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeGraphEvidenceSearchQuery {
    Lexical(String),
    LiteralOrFormal(String),
}

impl RuntimeGraphEvidenceSearchQuery {
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Lexical(text) | Self::LiteralOrFormal(text) => text,
        }
    }

    pub(super) fn literal_or_formal_text(&self) -> Option<&str> {
        match self {
            Self::Lexical(_) => None,
            Self::LiteralOrFormal(text) => Some(text),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphProjectionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphDocumentLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphSubTypeHintRow {
    pub node_type: String,
    pub sub_type: String,
    pub occurrences: i64,
}
