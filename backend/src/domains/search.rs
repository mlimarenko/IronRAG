use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchChunkEmbedding {
    pub chunk_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedded_at: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchGraphNodeEmbedding {
    pub node_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedded_at: DateTime<Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub library_id: Uuid,
    pub query_text: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub subject_id: Uuid,
    pub score: f32,
    pub preview: Option<String>,
}
