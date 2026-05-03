use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use crate::{
    domains::{
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        query::{QueryVerificationState, QueryVerificationWarning, RuntimeQueryMode},
    },
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
        KnowledgeTechnicalFactRow,
    },
    infra::repositories::{RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::knowledge::runtime_read::ActiveRuntimeGraphProjection,
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
    pub summary: Option<String>,
    pub support_count: i32,
    pub score: Option<f32>,
}

impl RuntimeMatchedRelationship {
    pub(crate) fn claim_text(&self) -> String {
        format!("{} --{}--> {}", self.from_label, self.relation_type, self.to_label)
    }

    pub(crate) fn evidence_text(&self) -> Option<&str> {
        self.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    }

    pub(crate) fn context_line(&self) -> String {
        if let Some(summary) = self.evidence_text() {
            let mut line = format!(
                "[graph-edge evidence] evidence: {summary} | relation_hint: {}",
                self.claim_text()
            );
            if self.support_count > 1 {
                line.push_str(" | support_count: ");
                line.push_str(&self.support_count.to_string());
            }
            return line;
        }

        let mut line = format!("[graph-edge relation_hint] {}", self.claim_text());
        if self.support_count > 1 {
            line.push_str(" | support_count: ");
            line.push_str(&self.support_count.to_string());
        }
        line
    }

    pub(crate) fn reference_excerpt(&self) -> String {
        match self.evidence_text() {
            Some(summary) => {
                format!("evidence: {} | relation_hint: {}", summary, self.claim_text())
            }
            None => self.claim_text(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeChunkScoreKind {
    Relevance,
    DocumentIdentity,
    EntityBio,
    GraphEvidence,
    QueryIrFocus,
    SourceContext,
    FocusedDocument,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeMatchedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    /// Canonical revision the chunk was fetched from. Needed by the
    /// focused-document consolidation stage so it can group chunks by
    /// the revision they actually came from (not just by `document_id`,
    /// which could in principle span revisions during index swap).
    pub revision_id: Uuid,
    /// Position of the chunk inside its document's linear ordering.
    /// Consolidation uses this to compute contiguous anchor ranges
    /// around already-retrieved chunks and to sort winner chunks back
    /// into reading order for the LLM prompt.
    pub chunk_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_kind: Option<String>,
    pub document_label: String,
    pub excerpt: String,
    #[serde(skip_serializing)]
    pub score_kind: RuntimeChunkScoreKind,
    pub score: Option<f32>,
    #[serde(skip_serializing)]
    pub source_text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RuntimeRetrievedDocumentBrief {
    pub(crate) title: String,
    pub(crate) preview_excerpt: String,
    /// Canonical source pointer for the document. For web-ingested
    /// pages this carries the original URL so the assistant can
    /// quote it inline ("see <url>"). For file uploads it may be
    /// `None` or a logical reference like `file://<key>`. The value
    /// comes from `content_revision.source_uri` on the document's
    /// readable (or active) revision.
    pub(crate) source_uri: Option<String>,
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
    /// Distinct document titles represented by the final ranked chunk
    /// bundle. Unlike `retrieved_documents`, this list does not depend
    /// on preview loading and is the canonical title source for routing
    /// decisions that only need to know which documents shaped context.
    pub(crate) retrieved_context_document_titles: Vec<String>,
    /// Final ranked chunks that survived consolidation + truncation and
    /// actually shaped the answer context. Captured here so the turn
    /// layer can persist a chunk-to-execution audit trail in
    /// `query_chunk_reference` without having to reach back into the
    /// internal `RetrievalBundle`.
    pub(crate) chunk_references: Vec<QueryChunkReferenceSnapshot>,
    /// Final ranked chunks with their runtime evidence overlays intact.
    /// `query_chunk_reference` stores only stable row ids; answer preflight
    /// still needs the selected runtime text, score lane, and excerpts.
    pub(crate) context_chunks: Vec<RuntimeMatchedChunk>,
    /// Ordered source-unit rows selected for a typed source-slice
    /// request. These are structured blocks, not `knowledge_chunk`
    /// rows, so they stay out of `query_chunk_reference` but remain
    /// available for deterministic enumeration answers.
    pub(crate) ordered_source_units: Vec<RuntimeMatchedChunk>,
    /// Runtime graph evidence rows that did not necessarily hydrate to
    /// `knowledge_chunk` rows. They are already selected and ranked by the
    /// retrieval graph-evidence lane, so canonical answer assembly can render
    /// them without parsing the old bounded context string.
    pub(crate) graph_evidence_context_lines: Vec<String>,
}

/// Persisted chunk-to-execution reference snapshot. Mirrors the
/// `query_chunk_reference` table schema so the turn-layer insert is
/// a 1:1 mapping.
#[derive(Debug, Clone)]
pub(crate) struct QueryChunkReferenceSnapshot {
    pub(crate) chunk_id: Uuid,
    pub(crate) rank: i32,
    pub(crate) score: f64,
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
    /// Canonical IR produced by `QueryCompilerService`. Drives the
    /// verifier's strictness policy via `QueryIR::verification_level`
    /// instead of blanket suppression on every unsupported literal.
    pub(crate) query_ir: crate::domains::query_ir::QueryIR,
}

#[derive(Debug, Clone)]
pub(crate) struct AnswerVerificationStage {
    pub(crate) generation: AnswerGenerationStage,
    pub(crate) verification: RuntimeAnswerVerification,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAnswerVerification {
    pub(crate) state: QueryVerificationState,
    pub(crate) warnings: Vec<QueryVerificationWarning>,
    pub(crate) unsupported_literals: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalAnswerEvidence {
    pub(crate) bundle: Option<crate::infra::arangodb::context_store::KnowledgeContextBundleRow>,
    pub(crate) chunk_rows: Vec<KnowledgeChunkRow>,
    pub(crate) structured_blocks: Vec<KnowledgeStructuredBlockRow>,
    pub(crate) technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

/// Captures the billing-relevant fields of a live QueryCompiler LLM
/// call. `None` at the call site means the compiler served the IR from
/// cache or from a fallback path, so there is no token usage to bill.
#[derive(Debug, Clone)]
pub(crate) struct QueryCompileUsage {
    pub(crate) provider_kind: String,
    pub(crate) model_name: String,
    pub(crate) usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAnswerQueryResult {
    pub(crate) structured: RuntimeStructuredQueryResult,
    pub(crate) answer_context: String,
    pub(crate) embedding_usage: Option<QuestionEmbeddingResult>,
    /// Focused-document decision already applied to the retrieval bundle
    /// before answer context assembly. Answer routing must consume this
    /// instead of re-inferring focus from the retained tangential tail.
    pub(crate) consolidation: super::consolidation::ConsolidationDiagnostics,
    /// Canonical typed representation of the user's question produced by
    /// `QueryCompilerService`. Downstream stages (verification, ranking,
    /// answer generation) should read routing signals from this instead
    /// of re-classifying the raw question with keyword lists.
    pub(crate) query_ir: crate::domains::query_ir::QueryIR,
    /// Billing-relevant usage from the QueryCompiler LLM call, if any.
    /// `None` when the IR was served from cache or from a fallback
    /// path. Captured separately from `embedding_usage` because the
    /// two hit different bindings (`QueryCompile` vs `ExtractText`),
    /// different models, and different per-call costs.
    pub(crate) query_compile_usage: Option<QueryCompileUsage>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueryGraphIndex {
    projection: Arc<ActiveRuntimeGraphProjection>,
    node_positions: HashMap<Uuid, usize>,
    edge_positions: HashMap<Uuid, usize>,
}

impl QueryGraphIndex {
    #[must_use]
    pub(crate) fn new(
        projection: Arc<ActiveRuntimeGraphProjection>,
        node_positions: HashMap<Uuid, usize>,
        edge_positions: HashMap<Uuid, usize>,
    ) -> Self {
        Self { projection, node_positions, edge_positions }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::new(
            Arc::new(ActiveRuntimeGraphProjection { nodes: Vec::new(), edges: Vec::new() }),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[must_use]
    pub(crate) fn node(&self, node_id: Uuid) -> Option<&RuntimeGraphNodeRow> {
        self.node_positions.get(&node_id).and_then(|position| self.projection.nodes.get(*position))
    }

    #[must_use]
    pub(crate) fn edge(&self, edge_id: Uuid) -> Option<&RuntimeGraphEdgeRow> {
        self.edge_positions.get(&edge_id).and_then(|position| self.projection.edges.get(*position))
    }

    pub(crate) fn nodes(&self) -> impl Iterator<Item = &RuntimeGraphNodeRow> + '_ {
        self.projection.nodes.iter().filter(|node| self.node_positions.contains_key(&node.id))
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = &RuntimeGraphEdgeRow> + '_ {
        self.projection.edges.iter().filter(|edge| self.edge_positions.contains_key(&edge.id))
    }

    #[must_use]
    pub(crate) fn node_count(&self) -> usize {
        self.node_positions.len()
    }

    #[must_use]
    pub(crate) fn edge_count(&self) -> usize {
        self.edge_positions.len()
    }
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
    pub(crate) graph_evidence_context_lines: Vec<String>,
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

        window_text: None,

        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
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
