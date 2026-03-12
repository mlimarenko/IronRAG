use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvidenceSummary {
    pub retrieval_run_id: Option<Uuid>,
    pub weak_grounding: bool,
    pub references: Vec<String>,
    pub warning: Option<String>,
}
