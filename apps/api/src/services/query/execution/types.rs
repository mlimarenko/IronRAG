use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    domains::{
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        query::RuntimeQueryMode,
    },
    infra::arangodb::{
        document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
            KnowledgeTechnicalFactRow,
        },
        graph_store::{GraphViewEdgeWrite, GraphViewNodeWrite},
    },
    services::query::assistant_grounding::AssistantGroundingEvidence,
    services::query::planner::{QueryIntentProfile, RuntimeQueryPlan},
};

use super::embed::QuestionEmbeddingResult;
use super::technical_literals::TechnicalLiteralIntent;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeMatchedEntity {
    pub node_id: Uuid,
    pub label: String,
    pub node_type: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeMatchedRelationship {
    pub edge_id: Uuid,
    pub relation_type: String,
    pub from_node_id: Uuid,
    pub from_label: String,
    pub to_node_id: Uuid,
    pub to_label: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeMatchedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub document_label: String,
    pub excerpt: String,
    pub score: Option<f32>,
    #[serde(skip_serializing)]
    pub source_text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeRetrievedDocumentBrief {
    pub(crate) title: String,
    pub(crate) preview_excerpt: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeStructuredQueryReferenceCounts {
    pub(crate) entity_count: usize,
    pub(crate) relationship_count: usize,
    pub(crate) chunk_count: usize,
    pub(crate) graph_node_count: usize,
    pub(crate) graph_edge_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeStructuredQueryLibrarySummary {
    pub(crate) document_count: usize,
    pub(crate) graph_ready_count: usize,
    pub(crate) processing_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) graph_status: &'static str,
    pub(crate) recent_documents: Vec<RuntimeQueryRecentDocument>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeStructuredQueryDiagnostics {
    pub(crate) requested_mode: RuntimeQueryMode,
    pub(crate) planned_mode: RuntimeQueryMode,
    pub(crate) keywords: Vec<String>,
    pub(crate) high_level_keywords: Vec<String>,
    pub(crate) low_level_keywords: Vec<String>,
    pub(crate) top_k: usize,
    pub(crate) reference_counts: RuntimeStructuredQueryReferenceCounts,
    pub(crate) planning: crate::domains::query::QueryPlanningMetadata,
    pub(crate) rerank: crate::domains::query::RerankMetadata,
    pub(crate) context_assembly: crate::domains::query::ContextAssemblyMetadata,
    pub(crate) grouped_references: Vec<crate::domains::query::GroupedReference>,
    pub(crate) context_text: Option<String>,
    pub(crate) warning: Option<String>,
    pub(crate) warning_kind: Option<&'static str>,
    pub(crate) library_summary: Option<RuntimeStructuredQueryLibrarySummary>,
}

#[cfg(test)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueryExecutionReference {
    pub reference_id: uuid::Uuid,
    pub kind: String,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueryExecutionEnrichment {
    pub planning: crate::domains::query::QueryPlanningMetadata,
    pub rerank: crate::domains::query::RerankMetadata,
    pub context_assembly: crate::domains::query::ContextAssemblyMetadata,
    pub grouped_references: Vec<crate::domains::query::GroupedReference>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeStructuredQueryResult {
    pub(crate) planned_mode: RuntimeQueryMode,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
    pub(crate) intent_profile: QueryIntentProfile,
    pub(crate) context_text: String,
    pub(crate) technical_literals_text: Option<String>,
    pub(crate) technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) diagnostics: RuntimeStructuredQueryDiagnostics,
    pub(crate) retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAnswerQueryResult {
    pub(crate) answer: String,
    pub(crate) provider: ProviderModelSelection,
    pub(crate) usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct AnswerGenerationStage {
    pub(crate) intent_profile: QueryIntentProfile,
    pub(crate) canonical_answer_chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) canonical_evidence: CanonicalAnswerEvidence,
    pub(crate) assistant_grounding: AssistantGroundingEvidence,
    pub(crate) answer: String,
    pub(crate) provider: ProviderModelSelection,
    pub(crate) usage_json: serde_json::Value,
    /// Full text that was passed to the LLM as the grounded context. The
    /// verification step uses this to validate that backticked literals in
    /// the answer are at least mentioned somewhere in what the LLM saw,
    /// including library summary lines and document metadata that aren't
    /// part of the chunk corpus.
    pub(crate) prompt_context: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AnswerVerificationStage {
    pub(crate) generation: AnswerGenerationStage,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalAnswerEvidence {
    pub(crate) bundle: Option<crate::infra::arangodb::context_store::KnowledgeContextBundleRow>,
    pub(crate) chunk_rows: Vec<KnowledgeChunkRow>,
    pub(crate) structured_blocks: Vec<KnowledgeStructuredBlockRow>,
    pub(crate) technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAnswerQueryResult {
    pub(crate) structured: RuntimeStructuredQueryResult,
    pub(crate) answer_context: String,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueryGraphIndex {
    pub(crate) nodes: HashMap<Uuid, GraphViewNodeWrite>,
    pub(crate) edges: HashMap<Uuid, GraphViewEdgeWrite>,
}

#[derive(Debug, Clone)]
pub(crate) struct RetrievalBundle {
    pub(crate) entities: Vec<RuntimeMatchedEntity>,
    pub(crate) relationships: Vec<RuntimeMatchedRelationship>,
    pub(crate) chunks: Vec<RuntimeMatchedChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeQueryWarning {
    pub(crate) warning: String,
    pub(crate) warning_kind: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeQueryLibrarySummary {
    pub(crate) document_count: usize,
    pub(crate) graph_ready_count: usize,
    pub(crate) processing_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) graph_status: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeQueryRecentDocument {
    pub(crate) title: String,
    pub(crate) uploaded_at: String,
    pub(crate) mime_type: Option<String>,
    pub(crate) pipeline_state: &'static str,
    pub(crate) graph_state: &'static str,
    pub(crate) preview_excerpt: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeQueryLibraryContext {
    pub(crate) summary: RuntimeQueryLibrarySummary,
    pub(crate) recent_documents: Vec<RuntimeQueryRecentDocument>,
    pub(crate) warning: Option<RuntimeQueryWarning>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeVectorSearchContext {
    pub(crate) model_catalog_id: Uuid,
    pub(crate) freshness_generation: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryPlanningStage {
    pub(crate) provider_profile: EffectiveProviderProfile,
    pub(crate) planning: crate::domains::query::QueryPlanningMetadata,
    pub(crate) plan: RuntimeQueryPlan,
    pub(crate) technical_literal_intent: TechnicalLiteralIntent,
    pub(crate) question_embedding: Vec<f32>,
    pub(crate) hyde_embedding: Option<Vec<f32>>,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
    pub(crate) graph_index: QueryGraphIndex,
    pub(crate) document_index: HashMap<Uuid, KnowledgeDocumentRow>,
    pub(crate) candidate_limit: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryRetrievalStage {
    pub(crate) planning: StructuredQueryPlanningStage,
    pub(crate) bundle: RetrievalBundle,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryRerankStage {
    pub(crate) retrieval: StructuredQueryRetrievalStage,
    pub(crate) rerank: crate::domains::query::RerankMetadata,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryAssemblyStage {
    pub(crate) rerank: StructuredQueryRerankStage,
    pub(crate) context_text: String,
    pub(crate) technical_literals_text: Option<String>,
    pub(crate) technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
    pub(crate) grouped_references: Vec<crate::domains::query::GroupedReference>,
    pub(crate) context_assembly: crate::domains::query::ContextAssemblyMetadata,
}

#[cfg(test)]
pub(crate) fn sample_chunk_row(
    chunk_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        key: chunk_id.to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        content_text: "chunk".to_string(),
        normalized_text: "chunk".to_string(),
        span_start: Some(0),
        span_end: Some(5),
        token_count: Some(1),
        support_block_ids: Vec::new(),
        section_path: vec!["root".to_string()],
        heading_trail: vec!["Root".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,
    }
}

#[cfg(test)]
pub(crate) fn sample_structured_block_row(
    block_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> KnowledgeStructuredBlockRow {
    let now = chrono::Utc::now();
    KnowledgeStructuredBlockRow {
        key: block_id.to_string(),
        arango_id: None,
        arango_rev: None,
        block_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        ordinal: 0,
        block_kind: "paragraph".to_string(),
        text: "segment".to_string(),
        normalized_text: "segment".to_string(),
        heading_trail: vec!["Root".to_string()],
        section_path: vec!["root".to_string()],
        page_number: Some(1),
        span_start: Some(0),
        span_end: Some(7),
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
pub(crate) fn sample_technical_fact_row(
    fact_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> KnowledgeTechnicalFactRow {
    let now = chrono::Utc::now();
    KnowledgeTechnicalFactRow {
        key: fact_id.to_string(),
        arango_id: None,
        arango_rev: None,
        fact_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        fact_kind: "endpoint_path".to_string(),
        canonical_value_text: "/health".to_string(),
        canonical_value_exact: "/health".to_string(),
        canonical_value_json: serde_json::json!("/health"),
        display_value: "/health".to_string(),
        qualifiers_json: serde_json::json!({}),
        support_block_ids: Vec::new(),
        support_chunk_ids: Vec::new(),
        confidence: Some(0.95),
        extraction_kind: "parser_first".to_string(),
        conflict_group_id: None,
        created_at: now,
        updated_at: now,
    }
}
