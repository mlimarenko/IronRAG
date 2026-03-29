use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractContent {
    pub revision_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub extract_state: String,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub warning_count: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractChunkResult {
    pub id: Uuid,
    pub chunk_id: Uuid,
    pub attempt_id: Uuid,
    pub extract_state: String,
    pub provider_call_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractNodeCandidate {
    pub id: Uuid,
    pub chunk_result_id: Uuid,
    pub canonical_key: String,
    pub node_kind: String,
    pub display_label: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractEdgeCandidate {
    pub id: Uuid,
    pub chunk_result_id: Uuid,
    pub canonical_key: String,
    pub edge_kind: String,
    pub from_canonical_key: String,
    pub to_canonical_key: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractResumeCursor {
    pub attempt_id: Uuid,
    pub last_completed_chunk_index: i32,
    pub replay_count: i32,
    pub downgrade_level: i32,
    pub updated_at: DateTime<Utc>,
}
