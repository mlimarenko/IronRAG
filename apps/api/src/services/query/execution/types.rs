use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use crate::{
    agent_runtime::tasks::query_answer::QueryProviderCall,
    domains::query::{QueryVerificationState, QueryVerificationWarning, RuntimeQueryMode},
    infra::knowledge_rows::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
        KnowledgeTechnicalFactRow,
    },
    infra::repositories::{RuntimeGraphQueryEdgeRow, RuntimeGraphQueryNodeRow},
    services::knowledge::runtime_read::ActiveRuntimeGraphProjection,
    services::query::assistant_grounding::AssistantGroundingEvidence,
    services::query::planner::{QueryIntentProfile, RuntimeQueryPlan},
};

use super::chunk_support::ContextExcerpt;
use super::technical_literals::TechnicalLiteralIntent;

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeMatchedEntity {
    pub node_id: Uuid,
    pub label: String,
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub score: Option<f32>,
}

impl RuntimeMatchedEntity {
    pub(super) fn context_excerpt(&self) -> Option<ContextExcerpt<'_>> {
        const ENTITY_SUMMARY_CONTEXT_CHARS: usize = 320;
        let excerpt = ContextExcerpt::new(self.summary.as_deref()?, ENTITY_SUMMARY_CONTEXT_CHARS);
        (!excerpt.source.is_empty()).then_some(excerpt)
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
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

/// Longest statement the grounded-answer contract protects verbatim
/// (`mustPreserveSpans`). Longer graph summaries stay ordinary evidence.
pub(crate) const PROTECTED_STATEMENT_MAX_CHARS: usize = 240;

pub(crate) fn protected_statement(text: &str) -> Option<&str> {
    let text = text.trim();
    (text.chars().count() <= PROTECTED_STATEMENT_MAX_CHARS
        && text.chars().any(char::is_alphanumeric))
    .then_some(text)
}

/// Canonical owner for applicable verbatim source statements shared by
/// execution verification and MCP `mustPreserveSpans` serialization.
///
/// A candidate statement is selected only when it appears verbatim inside an
/// authorized primary source passage cited by completed semantic verification
/// and matches the question's content tokens. It never filters by whether the
/// draft answer already included the statement, and never admits synthesized
/// graph summaries, titles, or unrelated peripheral quotes.
pub(crate) fn applicable_preserved_source_spans<'a>(
    question: &str,
    candidate_statements: impl IntoIterator<Item = &'a str>,
    semantic_verification: Option<
        &ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport,
    >,
    limit: usize,
) -> Vec<String> {
    use ironrag_contracts::assistant::semantic_verification::{
        SemanticClaimSupport, SemanticVerificationOutcome,
    };
    if limit == 0 {
        return Vec::new();
    }
    let Some(report) = semantic_verification else {
        return Vec::new();
    };
    let SemanticVerificationOutcome::Completed { assessment } = &report.outcome else {
        return Vec::new();
    };
    let cited_quotes: Vec<&str> = assessment
        .units
        .iter()
        .filter(|unit| unit.support != SemanticClaimSupport::NoFactualClaims)
        .flat_map(|unit| unit.evidence.iter().map(|link| link.quote.as_str()))
        .collect();
    if cited_quotes.is_empty() {
        return Vec::new();
    }
    let question_tokens = crate::services::query::text_match::normalized_alnum_tokens(question, 4);
    if question_tokens.is_empty() {
        return Vec::new();
    }
    let mut selected = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for candidate in candidate_statements {
        if selected.len() >= limit {
            break;
        }
        let Some(span) = protected_statement(candidate) else {
            continue;
        };
        if !cited_quotes.iter().any(|quote| quote.contains(span)) {
            continue;
        }
        if !statement_is_applicable_to_question(&question_tokens, span) {
            continue;
        }
        if seen.insert(span.to_owned()) {
            selected.push(span.to_owned());
        }
    }
    selected
}

fn statement_is_applicable_to_question(
    question_tokens: &std::collections::BTreeSet<String>,
    statement: &str,
) -> bool {
    let statement_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(statement, 4);
    if statement_tokens.is_empty() {
        return false;
    }
    let matched = question_tokens
        .iter()
        .filter(|q_token| {
            statement_tokens
                .iter()
                .any(|s_token| preserved_statement_tokens_match(q_token, s_token))
        })
        .count();
    matched >= 2
}

fn preserved_statement_tokens_match(left: &str, right: &str) -> bool {
    if crate::services::query::text_match::near_token_match(left, right) {
        return true;
    }
    let min_len = left.chars().count().min(right.chars().count());
    if min_len < 4 {
        return false;
    }
    let common_prefix = left.chars().zip(right.chars()).take_while(|(a, b)| a == b).count();
    common_prefix >= 4 && common_prefix.saturating_mul(5) >= min_len.saturating_mul(3)
}

impl RuntimeMatchedRelationship {
    pub(crate) fn claim_text(&self) -> String {
        format!("{} --{}--> {}", self.from_label, self.relation_type, self.to_label)
    }

    pub(crate) fn evidence_text(&self) -> Option<&str> {
        self.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    }

    /// Field marking a recorded statement the answer must keep verbatim.
    pub(crate) const EXACT_WORDING_FIELD: &'static str = "exact_wording: yes";

    pub(crate) fn context_line(&self) -> String {
        if let Some(summary) = self.evidence_text() {
            let mut line = format!("[graph-edge evidence] evidence: {summary}");
            if protected_statement(summary).is_some() {
                line.push_str(" | ");
                line.push_str(Self::EXACT_WORDING_FIELD);
            }
            line.push_str(" | relation_hint: ");
            line.push_str(&self.claim_text());
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeChunkScoreKind {
    Relevance,
    DocumentIdentity,
    EntityBio,
    GraphEvidence,
    QueryIrFocus,
    ContentAnchor,
    SourceContext,
    FocusedDocument,
    LatestVersion,
}

impl RuntimeChunkScoreKind {
    pub(super) const fn can_yield_overview_slot(self) -> bool {
        matches!(self, Self::Relevance | Self::SourceContext)
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeMatchedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    /// Current revision the chunk was fetched from. Needed by the
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

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeRetrievedDocumentBrief {
    pub(crate) title: String,
    pub(crate) preview_excerpt: String,
    /// LLM-visible document citation hint resolved from the revision's
    /// explicit hint, safe web URL fallback, or document title.
    pub(crate) document_hint: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeStructuredQueryReferenceCounts {
    pub(crate) entity_count: usize,
    pub(crate) relationship_count: usize,
    pub(crate) chunk_count: usize,
    pub(crate) graph_node_count: usize,
    pub(crate) graph_edge_count: usize,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RuntimeStructuredQueryLibrarySummary {
    pub(crate) document_count: usize,
    pub(crate) graph_ready_count: usize,
    pub(crate) processing_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) graph_status: &'static str,
    pub(crate) recent_documents: Vec<RuntimeQueryRecentDocument>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
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
    pub(crate) retrieval_degradations: Vec<RetrievalDegradation>,
    pub(crate) context_text: Option<String>,
    pub(crate) warning: Option<String>,
    pub(crate) warning_kind: Option<&'static str>,
    pub(crate) library_summary: Option<RuntimeStructuredQueryLibrarySummary>,
}

#[cfg(test)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct QueryExecutionReference {
    pub reference_id: uuid::Uuid,
    pub kind: String,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct QueryExecutionEnrichment {
    pub planning: crate::domains::query::QueryPlanningMetadata,
    pub rerank: crate::domains::query::RerankMetadata,
    pub context_assembly: crate::domains::query::ContextAssemblyMetadata,
    pub grouped_references: Vec<crate::domains::query::GroupedReference>,
}

/// One graph context line selected for the answer, paired with the evidence it
/// carries and with the producer that selected it. The producer owns both
/// representations while it formats the line, so no consumer has to strip
/// headers or attributes back out of the rendered text to recover the claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedGraphContextLine {
    rendered: String,
    source: SelectedGraphContextSource,
}

/// Which producer selected the line. A document graph-evidence passage is a
/// claim about a document; a matched entity is a graph node whose summary
/// describes the node itself. Consumers that accept only document passages ask
/// for the passage body instead of reading every summary as one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedGraphContextSource {
    /// `graph_evidence` row: the shortened body is always evidence.
    Passage(String),
    /// Explicit target node: only the summary excerpt used is evidence.
    MatchedEntity(Option<String>),
}

impl SelectedGraphContextLine {
    /// Document graph-evidence line: `evidence_text` is the shortened body the
    /// producer formatted into `rendered`.
    pub(crate) fn passage(rendered: String, evidence_text: String) -> Self {
        Self { rendered, source: SelectedGraphContextSource::Passage(evidence_text) }
    }

    /// Matched entity line: `evidence_text` is the summary excerpt the producer
    /// formatted into `rendered`, or `None` for a navigation-only line.
    pub(crate) fn matched_entity(rendered: String, evidence_text: Option<String>) -> Self {
        Self { rendered, source: SelectedGraphContextSource::MatchedEntity(evidence_text) }
    }

    /// Line as it enters the answer context.
    pub(crate) fn rendered(&self) -> &str {
        &self.rendered
    }

    /// Evidence the line carries: the shortened claim body of a graph-evidence
    /// row or the summary excerpt used for a matched-entity row.
    /// Navigation-only lines carry none.
    pub(crate) fn evidence_text(&self) -> Option<&str> {
        match &self.source {
            SelectedGraphContextSource::Passage(evidence_text) => Some(evidence_text),
            SelectedGraphContextSource::MatchedEntity(evidence_text) => evidence_text.as_deref(),
        }
    }

    /// Body of a document graph-evidence passage. `None` for matched-entity
    /// lines: a node summary describes the node, it is not a document passage.
    pub(crate) fn passage_evidence_text(&self) -> Option<&str> {
        match &self.source {
            SelectedGraphContextSource::Passage(evidence_text) => Some(evidence_text),
            SelectedGraphContextSource::MatchedEntity(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeStructuredQueryResult {
    pub(crate) planned_mode: RuntimeQueryMode,
    pub(crate) intent_profile: QueryIntentProfile,
    pub(crate) context_text: String,
    pub(crate) technical_literals_text: Option<String>,
    pub(crate) technical_literal_chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) diagnostics: RuntimeStructuredQueryDiagnostics,
    pub(crate) retrieved_documents: Vec<RuntimeRetrievedDocumentBrief>,
    /// Distinct document titles represented by the final ranked chunk
    /// bundle. Unlike `retrieved_documents`, this list does not depend
    /// on preview loading and is the authoritative title source for routing
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
    /// retrieval graph-evidence lane, so authoritative answer assembly can render
    /// them without parsing the old bounded context string. Each entry keeps the
    /// evidence text apart from its presentation.
    pub(crate) graph_evidence_context_lines: Vec<SelectedGraphContextLine>,
    /// Final ranked graph entities that shaped the answer context. These feed
    /// the persisted context bundle so execution detail references stay aligned
    /// with the evidence the answer actually saw.
    pub(crate) graph_entity_references: Vec<RuntimeMatchedEntity>,
    /// Final ranked graph relations that shaped the answer context.
    pub(crate) graph_relation_references: Vec<RuntimeMatchedRelationship>,
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
    pub(crate) provider_calls: Vec<QueryProviderCall>,
}

#[derive(Debug)]
pub(crate) struct RuntimeAnswerQueryFailure {
    pub(crate) error: anyhow::Error,
    /// Provider responses completed before verification, snapshot, or other
    /// post-generation work failed.
    pub(crate) provider_calls: Vec<QueryProviderCall>,
}

#[derive(Debug, Clone)]
pub(crate) struct AnswerGenerationStage {
    pub(crate) intent_profile: QueryIntentProfile,
    pub(crate) primary_answer_chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) authoritative_evidence: AuthoritativeAnswerEvidence,
    pub(crate) assistant_grounding: AssistantGroundingEvidence,
    pub(crate) answer: String,
    pub(crate) usage_json: serde_json::Value,
    /// Full text that was passed to the LLM as the grounded context. The
    /// verification step uses this to validate that backticked literals in
    /// the answer are at least mentioned somewhere in what the LLM saw,
    /// including library summary lines and document metadata that aren't
    /// part of the chunk corpus.
    pub(crate) prompt_context: String,
    /// Compiled IR produced by `QueryCompilerService`. Drives the
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
    pub(crate) semantic_verification:
        Option<ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport>,
    pub(crate) state: QueryVerificationState,
    pub(crate) warnings: Vec<QueryVerificationWarning>,
    pub(crate) unsupported_literals: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthoritativeAnswerEvidence {
    pub(crate) bundle: Option<crate::infra::knowledge_rows::KnowledgeContextBundleRow>,
    pub(crate) chunk_rows: Vec<KnowledgeChunkRow>,
    pub(crate) structured_blocks: Vec<KnowledgeStructuredBlockRow>,
    pub(crate) technical_facts: Vec<KnowledgeTechnicalFactRow>,
}

/// Durable execution correlation for optional semantic-rerank provider calls.
/// A provider mode is not allowed to start a paid call without this context.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SemanticRerankExecutionContext {
    pub(crate) workspace_id: Uuid,
    pub(crate) query_execution_id: Uuid,
    pub(crate) runtime_execution_id: Uuid,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAnswerQueryResult {
    pub(crate) structured: RuntimeStructuredQueryResult,
    pub(crate) answer_context: String,
    /// The final warning owner reports partial coverage when evidence was dropped.
    pub(crate) evidence_selection:
        Option<super::semantic_verification::SelectedEvidenceMeasurement>,
    /// Compiled typed representation of the user's question produced by
    /// `QueryCompilerService`. Downstream stages (verification, ranking,
    /// answer generation) should read routing signals from this instead
    /// of re-classifying the raw question with keyword lists.
    pub(crate) query_ir: crate::domains::query_ir::QueryIR,
    /// Fine-grained timed sub-operations (DB queries, retrieval lanes, …)
    /// captured while preparing this query, for the debug inspector. The answer
    /// stage reads these by reference when it builds this execution's snapshot,
    /// so every write of that row carries them.
    pub(crate) retrieval_spans: Vec<crate::services::query::turn_spans::TurnSpan>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueryGraphIndex {
    projection: Arc<ActiveRuntimeGraphProjection>,
    node_positions: HashMap<Uuid, usize>,
    edge_positions: HashMap<Uuid, usize>,
    incident_edge_ids: HashMap<Uuid, Vec<Uuid>>,
}

impl QueryGraphIndex {
    #[must_use]
    pub(crate) fn new(
        projection: Arc<ActiveRuntimeGraphProjection>,
        node_positions: HashMap<Uuid, usize>,
        edge_positions: HashMap<Uuid, usize>,
    ) -> Self {
        let mut incident_edge_ids = HashMap::<Uuid, Vec<Uuid>>::new();
        for edge in &projection.edges {
            if !edge_positions.contains_key(&edge.id)
                || !node_positions.contains_key(&edge.from_node_id)
                || !node_positions.contains_key(&edge.to_node_id)
            {
                continue;
            }
            incident_edge_ids.entry(edge.from_node_id).or_default().push(edge.id);
            incident_edge_ids.entry(edge.to_node_id).or_default().push(edge.id);
        }
        Self { projection, node_positions, edge_positions, incident_edge_ids }
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
    pub(crate) fn node(&self, node_id: Uuid) -> Option<&RuntimeGraphQueryNodeRow> {
        self.node_positions.get(&node_id).and_then(|position| self.projection.nodes.get(*position))
    }

    #[must_use]
    pub(crate) fn edge(&self, edge_id: Uuid) -> Option<&RuntimeGraphQueryEdgeRow> {
        self.edge_positions.get(&edge_id).and_then(|position| self.projection.edges.get(*position))
    }

    pub(crate) fn nodes(&self) -> impl Iterator<Item = &RuntimeGraphQueryNodeRow> + '_ {
        self.projection.nodes.iter().filter(|node| self.node_positions.contains_key(&node.id))
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = &RuntimeGraphQueryEdgeRow> + '_ {
        self.projection.edges.iter().filter(|edge| self.edge_positions.contains_key(&edge.id))
    }

    pub(crate) fn incident_edges(
        &self,
        node_id: Uuid,
    ) -> impl Iterator<Item = &RuntimeGraphQueryEdgeRow> + '_ {
        self.incident_edge_ids
            .get(&node_id)
            .into_iter()
            .flat_map(|edge_ids| edge_ids.iter())
            .filter_map(|edge_id| self.edge(*edge_id))
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

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
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
    pub(crate) embedding_profile_key: String,
    pub(crate) dimensions: u64,
    pub(crate) active_vector_generation: i64,
    pub(crate) source_truth_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryVectorReadiness {
    Ready,
    EmptyInventory,
    ProviderUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) enum RetrievalDegradation {
    #[serde(rename = "vector_provider_unavailable")]
    VectorProvider,
    #[serde(rename = "vector_store_unavailable")]
    VectorStore,
    #[serde(rename = "lexical_store_unavailable")]
    LexicalStore,
    #[serde(rename = "graph_store_unavailable")]
    GraphStore,
}

impl RetrievalDegradation {
    pub(crate) const fn warning_code(self) -> &'static str {
        match self {
            Self::VectorProvider => "retrieval_vector_provider_unavailable",
            Self::VectorStore => "retrieval_vector_store_unavailable",
            Self::LexicalStore => "retrieval_lexical_store_unavailable",
            Self::GraphStore => "retrieval_graph_store_unavailable",
        }
    }

    pub(crate) fn warning(self) -> QueryVerificationWarning {
        QueryVerificationWarning {
            code: self.warning_code().into(),
            message: "An optional retrieval channel was unavailable; the answer uses only the remaining authorized evidence.".into(),
            related_segment_id: None,
            related_fact_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryPlanningStage {
    pub(crate) planning: crate::domains::query::QueryPlanningMetadata,
    pub(crate) plan: RuntimeQueryPlan,
    pub(crate) technical_literal_intent: TechnicalLiteralIntent,
    pub(crate) question_embedding: Vec<f32>,
    pub(crate) vector_readiness: QueryVectorReadiness,
    pub(crate) hyde_embedding: Option<Vec<f32>>,
    /// Request-scoped exact-profile inventory preflight. Both chunk and
    /// entity ANN lanes consume this same immutable result; neither lane may
    /// rescan the full vector inventory or infer dimensions independently.
    pub(crate) vector_search_context: Option<RuntimeVectorSearchContext>,
    pub(crate) graph_index: QueryGraphIndex,
    pub(crate) graph_store_unavailable: bool,
    pub(crate) document_index: HashMap<Uuid, KnowledgeDocumentRow>,
    pub(crate) candidate_limit: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryRetrievalStage {
    pub(crate) planning: StructuredQueryPlanningStage,
    pub(crate) bundle: RetrievalBundle,
    pub(crate) graph_evidence_context_lines: Vec<SelectedGraphContextLine>,
    pub(crate) graph_evidence_source_document_ids: Vec<Uuid>,
    pub(crate) degradations: Vec<RetrievalDegradation>,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryRerankStage {
    pub(crate) retrieval: StructuredQueryRetrievalStage,
    pub(crate) rerank: crate::domains::query::RerankMetadata,
    /// Provider-supplied order for the scored chunk prefix. This sidecar keeps
    /// semantic rank distinct from the immutable retrieval/provenance score.
    pub(crate) semantic_chunk_ranks: HashMap<Uuid, usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredQueryAssemblyStage {
    pub(crate) rerank: StructuredQueryRerankStage,
    pub(crate) context_text: String,
    pub(crate) graph_evidence_context_lines: Vec<SelectedGraphContextLine>,
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
        fact_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        fact_kind: "endpoint_path".to_string(),
        normalized_value_text: "/health".to_string(),
        normalized_value_exact: "/health".to_string(),
        normalized_value_json: serde_json::json!("/health"),
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

#[cfg(test)]
mod preserved_span_tests {
    use ironrag_contracts::assistant::semantic_verification::{
        SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAnswerUnit, SemanticAssessment,
        SemanticByteSpan, SemanticClaimSupport, SemanticCompleteness, SemanticEvidenceLink,
        SemanticEvidenceScope, SemanticNotRunReason, SemanticResponseClassification,
        SemanticSourceEntry, SemanticSourceLocator, SemanticUnitVerdict,
        SemanticVerificationOutcome, SemanticVerificationReport,
    };

    use super::*;

    fn sample_report(
        outcome: SemanticVerificationOutcome,
        source_text: &str,
    ) -> SemanticVerificationReport {
        SemanticVerificationReport {
            protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
            question_sha256: "a".repeat(64),
            checked_answer_sha256: "b".repeat(64),
            visible_answer_sha256: Some("b".repeat(64)),
            evidence_manifest_sha256: "c".repeat(64),
            answer_units: vec![SemanticAnswerUnit {
                unit_id: "u1".to_string(),
                span: SemanticByteSpan { start: 0, end: 5 },
                text: "claim".to_string(),
            }],
            sources: vec![SemanticSourceEntry {
                source_id: "s1".to_string(),
                document_id: Uuid::from_u128(1),
                document_revision_id: Uuid::from_u128(2),
                locator: SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(3) },
                scope: SemanticEvidenceScope::SelectedExcerpt,
                text_sha256: "d".repeat(64),
                text: source_text.to_string(),
            }],
            elapsed_ms: 1,
            provider_call_id: None,
            preparation_limit: None,
            outcome,
        }
    }

    fn completed_report(support: SemanticClaimSupport, quote: &str) -> SemanticVerificationReport {
        sample_report(
            SemanticVerificationOutcome::Completed {
                assessment: SemanticAssessment {
                    claim_support: support,
                    completeness: SemanticCompleteness::Complete,
                    missing_caveats: Vec::new(),
                    response_classification: SemanticResponseClassification::FactualAnswer,
                    units: vec![SemanticUnitVerdict {
                        unit_id: "u1".to_string(),
                        support,
                        asserts_document_absence: false,
                        evidence: vec![SemanticEvidenceLink {
                            source_id: "s1".to_string(),
                            span: SemanticByteSpan { start: 0, end: quote.len() },
                            quote: quote.to_string(),
                        }],
                    }],
                },
            },
            quote,
        )
    }

    #[test]
    fn applicable_preserved_source_spans_handles_outcomes_no_factual_claims_and_cyrillic_stems() {
        let statement = "Параметры очереди обработки задач требуют явного таймаута.";
        let peripheral = "Архивный раздел описывает старый формат экспорта.";
        let question = "Какие параметры очередности обработки требуют таймаут?";

        let not_run = sample_report(
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::Disabled },
            statement,
        );
        assert!(
            applicable_preserved_source_spans(question, [statement], Some(&not_run), 8).is_empty()
        );

        let no_factual = completed_report(SemanticClaimSupport::NoFactualClaims, statement);
        assert!(
            applicable_preserved_source_spans(question, [statement], Some(&no_factual), 8)
                .is_empty()
        );

        let quote = format!("{statement} {peripheral}");
        let completed = completed_report(SemanticClaimSupport::Supported, &quote);
        assert_eq!(
            applicable_preserved_source_spans(
                question,
                [statement, peripheral, "Нецитируемая строка про параметры обработки."],
                Some(&completed),
                8,
            ),
            vec![statement.to_string()]
        );
    }
}
