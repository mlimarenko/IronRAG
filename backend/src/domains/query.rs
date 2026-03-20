use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
