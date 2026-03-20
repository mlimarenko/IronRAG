use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::graph_quality::{
    CanonicalGraphSummary, ExtractionRecoverySummary, MutationImpactScopeSummary,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeNodeType {
    Document,
    Entity,
    Topic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphStatus {
    Empty,
    Building,
    Ready,
    Partial,
    Failed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphReconciliationStatus {
    Current,
    Updating,
    Deleting,
    Mixed,
    Failed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphConvergenceStatus {
    Partial,
    Current,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphArtifactFilterReason {
    EmptyRelation,
    DegenerateSelfLoop,
    LowValueArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphProjectionHealth {
    Healthy,
    RetryingContention,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphProjectionLockState {
    Idle,
    Acquired,
    RetryingContention,
    FailedContention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphProjectionWriteState {
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphWriteFailureKind {
    ProjectionContention,
    GraphPersistenceIntegrity,
    DiagnosticsUnavailable,
    ProjectionFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphNode {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub label: String,
    pub node_type: RuntimeNodeType,
    pub support_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphEdge {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub support_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphEvidence {
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f32>,
    pub active_provenance_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphArtifactCounters {
    pub filtered_artifact_count: usize,
    pub filtered_empty_relation_count: usize,
    pub filtered_degenerate_loop_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphSnapshot {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub graph_status: RuntimeGraphStatus,
    pub reconciliation_status: RuntimeGraphReconciliationStatus,
    pub convergence_status: RuntimeGraphConvergenceStatus,
    pub node_count: usize,
    pub edge_count: usize,
    pub projection_version: i64,
    pub pending_update_count: usize,
    pub pending_delete_count: usize,
    pub last_mutation_warning: Option<String>,
    pub active_provenance_only: bool,
    pub filtered_artifacts: RuntimeGraphArtifactCounters,
    pub built_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphQuality {
    pub canonical_summary: Option<CanonicalGraphSummary>,
    pub extraction_recovery: Option<ExtractionRecoverySummary>,
    pub reconciliation_scope: Option<MutationImpactScopeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphProjectionScope {
    pub id: Uuid,
    pub library_id: Uuid,
    pub scope_kind: String,
    pub attempt_no: i32,
    pub lock_state: RuntimeGraphProjectionLockState,
    pub write_state: RuntimeGraphProjectionWriteState,
    pub deadlock_retry_count: usize,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_kind: Option<RuntimeGraphWriteFailureKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeGraphDiagnosticsSnapshot {
    pub library_id: Uuid,
    pub snapshot_at: DateTime<Utc>,
    pub projection_health: RuntimeGraphProjectionHealth,
    pub active_projection_count: usize,
    pub retrying_projection_count: usize,
    pub failed_projection_count: usize,
    pub pending_node_write_count: usize,
    pub pending_edge_write_count: usize,
    pub last_projection_failure_kind: Option<RuntimeGraphWriteFailureKind>,
    pub last_projection_failure_at: Option<DateTime<Utc>>,
    pub is_runtime_readable: bool,
}
