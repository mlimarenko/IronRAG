use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::query_intelligence::{
    ContextAssemblyMetadata, GroupedReference, QueryPlanningMetadata, RerankMetadata,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvidenceSummary {
    pub retrieval_run_id: Option<Uuid>,
    pub weak_grounding: bool,
    pub references: Vec<String>,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
    pub diagnostics: Option<RetrievalDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalDiagnostics {
    pub planning: QueryPlanningMetadata,
    pub rerank: RerankMetadata,
    pub context_assembly: ContextAssemblyMetadata,
    pub grouped_references: Vec<GroupedReference>,
}
