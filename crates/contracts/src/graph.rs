//! Knowledge-graph topology and inspection contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Broad semantic class assigned to a graph node by extraction.
pub enum GraphNodeType {
    /// Entity without a more specific supported class.
    Entity,
    /// Individual person.
    Person,
    /// Organization or organized group.
    Organization,
    /// Physical or named location.
    Location,
    /// Event occurring at a point or interval in time.
    Event,
    /// Created physical or digital artifact.
    Artifact,
    /// Naturally occurring object or phenomenon.
    Natural,
    /// Process, workflow, or ordered activity.
    Process,
    /// Abstract concept.
    Concept,
    /// Property represented as a first-class node.
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Lifecycle state of a library's materialized graph.
pub enum GraphStatus {
    /// No graph data has been materialized.
    Empty,
    /// The first graph generation is being assembled.
    Building,
    /// A replacement generation is being assembled while an older one exists.
    Rebuilding,
    /// The active generation is complete and current.
    Ready,
    /// A usable graph exists but some source material is not represented.
    Partial,
    /// Graph materialization failed without a usable result.
    Failed,
    /// The active graph no longer matches the current source revisions.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Degree to which the active graph reflects current source revisions.
pub enum GraphConvergenceStatus {
    /// All eligible current revisions are represented.
    Current,
    /// Only part of the eligible revision set is represented.
    Partial,
    /// Convergence cannot complete because one or more graph operations failed.
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Identity and health of the latest known graph generation.
pub struct GraphGenerationSummary {
    /// Persistent generation identifier when materialization has started.
    pub generation_id: Option<Uuid>,
    /// Monotonic generation number currently serving graph reads.
    pub active_graph_generation: i64,
    /// Machine-readable degraded-state code, if the generation is impaired.
    pub degraded_state: Option<String>,
    /// Time at which the generation status last changed.
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Document coverage and generation readiness for one library graph.
pub struct GraphReadinessSummary {
    /// Library whose source revisions were counted.
    pub library_id: Uuid,
    /// Document totals grouped by readiness state.
    pub document_counts_by_readiness: Vec<(String, i64)>,
    /// Documents with graph materialization complete.
    pub graph_ready_document_count: i64,
    /// Documents represented by too little graph evidence for full readiness.
    pub graph_sparse_document_count: i64,
    /// Documents contributing at least one typed fact.
    pub typed_fact_document_count: i64,
    /// Most recent generation metadata when a generation exists.
    pub latest_generation: Option<GraphGenerationSummary>,
    /// Time at which this coverage snapshot was assembled.
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Compact node representation used to render graph topology.
pub struct GraphNode {
    /// Stable node identifier within the library graph.
    pub id: Uuid,
    /// Normalized identity used to converge equivalent extracted mentions.
    #[serde(rename = "\u{63}anonicalKey")]
    pub identity_key: String,
    /// Primary human-readable node label.
    pub label: String,
    /// Broad semantic class assigned during extraction.
    pub node_type: GraphNodeType,
    /// Optional disambiguating label shown alongside the primary label.
    pub secondary_label: Option<String>,
    /// Number of retained evidence records supporting this node.
    pub support_count: i32,
    /// Evidence-grounded summary when one has been materialized.
    pub summary: Option<String>,
    /// Whether the node is hidden by default as an extraction artifact.
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Directed relation between two graph nodes.
pub struct GraphEdge {
    /// Stable relation identifier within the library graph.
    pub id: Uuid,
    /// Normalized identity used to converge equivalent extracted relations.
    #[serde(rename = "\u{63}anonicalKey")]
    pub identity_key: String,
    /// Origin node of the directed relation.
    pub source: Uuid,
    /// Destination node of the directed relation.
    pub target: Uuid,
    /// Extracted or normalized relation label.
    pub relation_type: String,
    /// Number of retained evidence records supporting this relation.
    pub support_count: i32,
    /// Whether the relation is hidden by default as an extraction artifact.
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Topology, readiness, and diagnostics required by the graph viewer.
pub struct GraphSurface {
    /// Library represented by this graph snapshot.
    pub library_id: Uuid,
    /// Current graph materialization lifecycle.
    pub status: GraphStatus,
    /// Coverage of current source revisions when it can be determined.
    pub convergence_status: Option<GraphConvergenceStatus>,
    /// Operator-readable explanation of partial, failed, or stale state.
    pub warning: Option<String>,
    /// Total nodes available in the active view.
    pub node_count: i32,
    /// Number of distinct relation classes in the active view.
    pub relation_count: i32,
    /// Total directed edges available in the active view.
    pub edge_count: i32,
    /// Documents with complete graph materialization.
    pub graph_ready_document_count: i32,
    /// Documents represented by sparse graph evidence.
    pub graph_sparse_document_count: i32,
    /// Documents contributing typed facts to the active graph.
    pub typed_fact_document_count: i32,
    /// Time at which the graph snapshot was last refreshed.
    pub updated_at: Option<DateTime<Utc>>,
    /// Nodes included in the current topology response.
    pub nodes: Vec<GraphNode>,
    /// Directed relations included in the current topology response.
    pub edges: Vec<GraphEdge>,
    /// Detailed source coverage when requested or available.
    pub readiness_summary: Option<GraphReadinessSummary>,
}
