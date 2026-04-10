use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::agent_runtime::{RuntimeExecutionSummary, RuntimeLifecycleState, RuntimeStageKind},
    domains::content::ContentSourceAccess,
    shared::extraction::{
        structured_document::StructuredBlockKind, technical_facts::TechnicalFactKind,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeQueryMode {
    Document,
    Local,
    Global,
    Hybrid,
    Mix,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryConversationState {
    Active,
    Archived,
}

impl QueryConversationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl std::str::FromStr for QueryConversationState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            other => Err(format!("unsupported query conversation state: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryTurnKind {
    User,
    Assistant,
    System,
    Tool,
}

impl QueryTurnKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::str::FromStr for QueryTurnKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            other => Err(format!("unsupported query turn kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRuntimeStageSummary {
    pub stage_kind: RuntimeStageKind,
    pub stage_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConversation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: QueryConversationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTurn {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: QueryTurnKind,
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
    pub runtime_execution_id: Option<Uuid>,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
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
    pub label: String,
    pub entity_type: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryGraphEdgeReference {
    pub execution_id: Uuid,
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentReference {
    pub execution_id: Uuid,
    pub segment_id: Uuid,
    pub revision_id: Uuid,
    pub block_kind: StructuredBlockKind,
    pub rank: i32,
    pub score: f64,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub document_id: Option<Uuid>,
    pub document_title: Option<String>,
    pub source_uri: Option<String>,
    pub source_access: Option<ContentSourceAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactReference {
    pub execution_id: Uuid,
    pub fact_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: TechnicalFactKind,
    pub canonical_value: String,
    pub display_value: String,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QueryVerificationState {
    #[default]
    NotRun,
    Verified,
    PartiallySupported,
    Conflicting,
    InsufficientEvidence,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerificationWarning {
    pub code: String,
    pub message: String,
    pub related_segment_id: Option<Uuid>,
    pub related_fact_id: Option<Uuid>,
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
    pub runtime_summary: RuntimeExecutionSummary,
    pub runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    pub request_turn: Option<QueryTurn>,
    pub response_turn: Option<QueryTurn>,
    pub chunk_references: Vec<QueryChunkReference>,
    pub prepared_segment_references: Vec<PreparedSegmentReference>,
    pub technical_fact_references: Vec<TechnicalFactReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
    pub verification_state: QueryVerificationState,
    pub verification_warnings: Vec<QueryVerificationWarning>,
}
