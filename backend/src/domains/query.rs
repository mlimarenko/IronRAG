use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeQueryMode {
    Document,
    Local,
    Global,
    Hybrid,
    Mix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryTurnStreamStage {
    Retrieving,
    Grounding,
    Answering,
}

impl RuntimeQueryMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Local => "local",
            Self::Global => "global",
            Self::Hybrid => "hybrid",
            Self::Mix => "mix",
        }
    }
}

impl std::str::FromStr for RuntimeQueryMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "document" => Ok(Self::Document),
            "local" => Ok(Self::Local),
            "global" => Ok(Self::Global),
            "hybrid" => Ok(Self::Hybrid),
            "mix" => Ok(Self::Mix),
            other => Err(format!("unsupported query mode: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntentCacheStatus {
    Miss,
    HitFresh,
    HitStaleRecomputed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentKeywords {
    pub high_level: Vec<String>,
    pub low_level: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanningMetadata {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub intent_cache_status: QueryIntentCacheStatus,
    pub keywords: IntentKeywords,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankStatus {
    NotApplicable,
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankMetadata {
    pub status: RerankStatus,
    pub candidate_count: usize,
    pub reordered_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAssemblyStatus {
    DocumentOnly,
    GraphOnly,
    BalancedMixed,
    MixedSkewed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAssemblyMetadata {
    pub status: ContextAssemblyStatus,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupedReferenceKind {
    Document,
    Relationship,
    Entity,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupedReference {
    pub id: String,
    pub kind: GroupedReferenceKind,
    pub rank: usize,
    pub title: String,
    pub excerpt: Option<String>,
    pub evidence_count: usize,
    pub support_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConversation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTurn {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: String,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub execution_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecution {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub context_bundle_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub execution_state: String,
    pub query_text: String,
    pub failure_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryChunkReference {
    pub execution_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryGraphNodeReference {
    pub execution_id: Uuid,
    pub node_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryGraphEdgeReference {
    pub execution_id: Uuid,
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConversationDetail {
    pub conversation: QueryConversation,
    pub turns: Vec<QueryTurn>,
    pub executions: Vec<QueryExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecutionDetail {
    pub execution: QueryExecution,
    pub request_turn: Option<QueryTurn>,
    pub response_turn: Option<QueryTurn>,
    pub chunk_references: Vec<QueryChunkReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
}
