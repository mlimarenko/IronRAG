use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphProjection {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_attempt_id: Option<Uuid>,
    pub projection_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: Uuid,
    pub projection_id: Uuid,
    pub canonical_key: String,
    pub node_kind: String,
    pub display_label: String,
    pub support_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: Uuid,
    pub projection_id: Uuid,
    pub canonical_key: String,
    pub edge_kind: String,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub support_count: i32,
}
