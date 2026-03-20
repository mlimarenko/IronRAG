use serde::Serialize;

use crate::domains::graph_quality::{
    CanonicalGraphSummary, ExtractionRecoverySummary, MutationImpactScopeSummary,
};
use crate::domains::query_intelligence::{
    ContextAssemblyMetadata, GroupedReference, QueryPlanningMetadata, RerankMetadata,
};
use crate::domains::ui_chat::{
    ChatSessionDetailModel, ChatSessionSettingsModel, ChatSessionSummaryModel,
};

#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeModel {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub secondary_label: Option<String>,
    pub support_count: i32,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdgeModel {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation_type: String,
    pub support_count: i32,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphLegendItemModel {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantProviderModel {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantReferenceModel {
    pub kind: String,
    pub reference_id: String,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantModeDescriptorModel {
    pub mode: String,
    pub label_key: String,
    pub short_description_key: String,
    pub best_for_key: String,
    pub caution_key: Option<String>,
    pub example_question_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantConfigModel {
    pub config_version: String,
    pub scope_hint_key: String,
    pub grouped_reference_semantics_key: String,
    pub default_prompt_keys: Vec<String>,
    pub modes: Vec<GraphAssistantModeDescriptorModel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantMessageModel {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub query_id: Option<String>,
    pub mode: Option<String>,
    pub grounding_status: Option<String>,
    pub provider: Option<GraphAssistantProviderModel>,
    pub references: Vec<GraphAssistantReferenceModel>,
    pub planning: Option<QueryPlanningMetadata>,
    pub rerank: Option<RerankMetadata>,
    pub context_assembly: Option<ContextAssemblyMetadata>,
    pub grouped_references: Vec<GroupedReference>,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantFocusContextModel {
    pub node_id: String,
    pub label: String,
    pub summary: String,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantModel {
    pub title: String,
    pub subtitle: String,
    pub prompts: Vec<String>,
    pub disclaimer: String,
    pub config: Option<GraphAssistantConfigModel>,
    pub session_id: Option<String>,
    pub recent_sessions: Vec<ChatSessionSummaryModel>,
    pub active_session: Option<ChatSessionDetailModel>,
    pub settings_summary: Option<ChatSessionSettingsModel>,
    pub focus_context: Option<GraphAssistantFocusContextModel>,
    pub messages: Vec<GraphAssistantMessageModel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSearchHitModel {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub secondary_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphRelatedEdgeModel {
    pub id: String,
    pub relation_type: String,
    pub other_node_id: String,
    pub other_node_label: String,
    pub support_count: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEvidenceModel {
    pub id: String,
    pub document_id: Option<String>,
    pub document_label: Option<String>,
    pub chunk_id: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: String,
    pub active_provenance_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeDetailModel {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub summary: String,
    pub properties: Vec<(String, String)>,
    pub related_documents: Vec<GraphSearchHitModel>,
    pub connected_nodes: Vec<GraphSearchHitModel>,
    pub related_edges: Vec<GraphRelatedEdgeModel>,
    pub evidence: Vec<GraphEvidenceModel>,
    pub relation_count: usize,
    pub canonical_summary: Option<CanonicalGraphSummary>,
    pub extraction_recovery: Option<ExtractionRecoverySummary>,
    pub reconciliation_scope: Option<MutationImpactScopeSummary>,
    pub reconciliation_status: Option<String>,
    pub convergence_status: Option<String>,
    pub pending_update_count: usize,
    pub pending_delete_count: usize,
    pub active_provenance_only: bool,
    pub filtered_artifact_count: Option<usize>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphAssistantAnswerModel {
    pub session_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub query_id: String,
    pub effective_mode: String,
    pub session_summary: Option<ChatSessionDetailModel>,
    pub settings_summary: Option<ChatSessionSettingsModel>,
    pub user_message: GraphAssistantMessageModel,
    pub assistant_message: GraphAssistantMessageModel,
    pub answer: String,
    pub references: Vec<String>,
    pub structured_references: Vec<GraphAssistantReferenceModel>,
    pub grouped_references: Vec<GroupedReference>,
    pub mode: String,
    pub grounding_status: String,
    pub provider: GraphAssistantProviderModel,
    pub planning: QueryPlanningMetadata,
    pub rerank: RerankMetadata,
    pub context_assembly: ContextAssemblyMetadata,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSurfaceModel {
    pub graph_status: String,
    pub convergence_status: Option<String>,
    pub projection_version: i64,
    pub node_count: usize,
    pub relation_count: usize,
    pub last_built_at: Option<String>,
    pub filtered_artifact_count: Option<usize>,
    pub warning: Option<String>,
    pub warnings: Vec<String>,
    pub nodes: Vec<GraphNodeModel>,
    pub edges: Vec<GraphEdgeModel>,
    pub legend: Vec<GraphLegendItemModel>,
    pub assistant: GraphAssistantModel,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphDiagnosticsModel {
    pub graph_status: String,
    pub reconciliation_status: String,
    pub convergence_status: Option<String>,
    pub projection_version: i64,
    pub node_count: usize,
    pub edge_count: usize,
    pub projection_freshness: String,
    pub rebuild_backlog_count: usize,
    pub ready_no_graph_count: usize,
    pub pending_update_count: usize,
    pub pending_delete_count: usize,
    pub active_mutation_scope: Option<MutationImpactScopeSummary>,
    pub filtered_artifact_count: Option<usize>,
    pub filtered_empty_relation_count: Option<usize>,
    pub filtered_degenerate_loop_count: Option<usize>,
    pub provenance_coverage_percent: Option<f64>,
    pub last_built_at: Option<String>,
    pub last_error_message: Option<String>,
    pub last_mutation_warning: Option<String>,
    pub active_provenance_only: bool,
    pub blockers: Vec<String>,
    pub warning: Option<String>,
    pub graph_backend: String,
}
