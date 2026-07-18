//! Document lifecycle and ingestion contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::{MessageLevel, OperatorWarning};
use crate::graph::GraphSurface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Highest query capability currently available for a document.
pub enum DocumentReadiness {
    /// Ingestion has not yet produced readable content.
    Processing,
    /// Extracted text can be read and searched lexically.
    Readable,
    /// Graph extraction completed with insufficient structured evidence.
    GraphSparse,
    /// Text, vectors, and graph evidence are available.
    GraphReady,
    /// The document could not reach a usable readiness state.
    Failed,
}

impl DocumentReadiness {
    #[must_use]
    /// Returns the canonical snake-case value used by storage and transport layers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Processing => "processing",
            Self::Readable => "readable",
            Self::GraphSparse => "graph_sparse",
            Self::GraphReady => "graph_ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Operational lifecycle of a document ingestion attempt.
pub enum DocumentStatus {
    /// Accepted and waiting for a worker.
    Queued,
    /// A worker is actively ingesting the document.
    Processing,
    /// The current ingestion attempt completed successfully.
    Ready,
    /// The current ingestion attempt ended in failure.
    Failed,
    /// Ingest was cancelled by operator or superseded by a newer
    /// mutation. Distinct from `Failed` — the operator started it
    /// and then withdrew, the document was not rejected by the
    /// pipeline. Rendered with neutral styling in the UI; mixing
    /// cancels into failures was masking real pipeline failures on
    /// the dashboard.
    Canceled,
}

impl DocumentStatus {
    #[must_use]
    /// Returns the canonical snake-case value used by storage and transport layers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Metadata for the document revision currently exposed by a detail view.
pub struct DocumentRevisionSummary {
    /// Stable revision identifier when a revision has been materialized.
    pub revision_id: Option<Uuid>,
    /// Monotonic revision number within the document.
    pub revision_number: Option<i64>,
    /// Detected or declared media type of the source payload.
    pub mime_type: Option<String>,
    /// Source payload size in bytes.
    pub byte_size: Option<i64>,
    /// Title extracted from or assigned to the revision.
    pub title: Option<String>,
    /// Language code produced by extraction when known.
    pub language_code: Option<String>,
    /// Original source location when the revision came from a URI.
    pub source_uri: Option<String>,
    /// Opaque object-storage key for the source payload.
    pub storage_key: Option<String>,
    /// Content checksum used for integrity and duplicate detection.
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Extracted structural segment prepared for chunking and inspection.
pub struct PreparedSegment {
    /// Stable segment identifier.
    pub id: Uuid,
    /// Zero-based position in source-document order.
    pub ordinal: i32,
    /// Structural block class reported by extraction.
    pub block_kind: String,
    /// Ordered heading ancestry surrounding the segment.
    pub heading_trail: Vec<String>,
    /// Bounded text preview for operator inspection.
    pub excerpt: String,
    /// One-based source page when pagination is available.
    pub page_number: Option<i32>,
    /// Search chunks derived from this segment.
    pub chunk_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Normalized, evidence-backed fact extracted from a document revision.
pub struct TechnicalFact {
    /// Stable fact identifier.
    pub id: Uuid,
    /// Typed fact category assigned by extraction.
    pub fact_kind: String,
    /// Source-oriented representation suitable for display.
    pub display_value: String,
    /// Normalized representation used for comparison and retrieval.
    pub canonical_value: String,
    /// Extraction confidence on the producer-defined normalized scale.
    pub confidence: Option<f64>,
    /// Structured context that narrows the fact's meaning.
    pub qualifiers: Vec<String>,
    /// Distinct chunks providing evidence for the fact.
    pub support_chunk_count: i32,
    /// Time at which the fact was materialized.
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Crawl identity and source URLs attached to a web-ingested document.
pub struct WebPageProvenance {
    /// Crawl run that discovered or refreshed the page.
    pub run_id: Option<Uuid>,
    /// Candidate record selected for materialization.
    pub candidate_id: Option<Uuid>,
    /// URI observed when the page was fetched.
    pub source_uri: Option<String>,
    /// Normalized URL used to identify duplicate pages.
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// List-view metadata and current processing state for one document.
pub struct DocumentSummary {
    /// Stable document identifier.
    pub id: Uuid,
    /// Owning workspace when included by the response surface.
    pub workspace_id: Option<Uuid>,
    /// Owning library when included by the response surface.
    pub library_id: Option<Uuid>,
    /// Display name derived from the source or upload.
    pub file_name: String,
    /// Source format or media-type label shown in document lists.
    pub file_type: String,
    /// Source payload size in bytes.
    pub file_size: i64,
    /// Time at which the document mutation was accepted.
    pub uploaded_at: DateTime<Utc>,
    /// Operational state of the current ingestion attempt.
    pub status: DocumentStatus,
    /// Highest query capability currently available.
    pub readiness: DocumentReadiness,
    /// Operator-facing name of the active ingestion stage.
    pub stage_label: Option<String>,
    /// Best-effort completion percentage for the active attempt.
    pub progress_percent: Option<i32>,
    /// Attributed provider cost in US dollars when accounting is available.
    pub cost_usd: Option<f64>,
    /// Sanitized explanation of the terminal failure.
    pub failure_message: Option<String>,
    /// Whether the current terminal state supports an operator retry.
    pub can_retry: bool,
    /// Extracted structural segment count when computed.
    pub prepared_segment_count: Option<i32>,
    /// Materialized typed-fact count when computed.
    pub technical_fact_count: Option<i32>,
    /// Normalized format selected by the extraction pipeline.
    pub source_format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Aggregate document counts used by list and dashboard summaries.
pub struct DocumentsOverview {
    /// All documents included in the current scope.
    pub total_documents: i32,
    /// Documents whose current attempt completed successfully.
    pub ready_documents: i32,
    /// Documents queued or actively processing.
    pub processing_documents: i32,
    /// Documents whose current attempt failed.
    pub failed_documents: i32,
    /// Ready documents with sparse structured graph evidence.
    pub graph_sparse_documents: i32,
}

/// Canonical per-library document metrics — one authoritative shape for all surfaces.
///
/// Every surface (`/ops/libraries/{id}/dashboard`,
/// `/content/libraries/{id}/documents?includeTotal=true`,
/// `/knowledge/libraries/{id}/summary`) MUST read from this shape.
///
/// Invariants enforced by the canonical aggregator:
///   * `total == ready + processing + queued + failed + canceled`
///   * `graph_ready + graph_sparse == ready` (graph readiness is a
///     split of the ready bucket; it does not overlap with processing
///     / queued / failed / canceled).
///
/// `in_flight` is intentionally NOT a field on this struct — it is a
/// derived value `processing + queued`. Adding it would invite the
/// same kind of double-counting drift we just finished removing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryDocumentMetrics {
    /// Sum of all mutually exclusive lifecycle buckets.
    pub total: i64,
    /// Successfully ingested documents.
    pub ready: i64,
    /// Documents with an active worker attempt.
    pub processing: i64,
    /// Documents waiting for a worker attempt.
    pub queued: i64,
    /// Documents whose current attempt failed.
    pub failed: i64,
    /// Documents whose current attempt was canceled or superseded.
    pub canceled: i64,
    /// Ready documents with complete graph materialization.
    pub graph_ready: i64,
    /// Ready documents with sparse graph materialization.
    pub graph_sparse: i64,
    /// Time at which the aggregate was recomputed.
    pub recomputed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Filters applied to a document list and detail surface.
pub struct DocumentFilterState {
    /// Optional lexical match over document display metadata.
    pub search_query: Option<String>,
    /// Operational lifecycle states included in the result.
    pub statuses: Vec<DocumentStatus>,
    /// Query-readiness states included in the result.
    pub readiness: Vec<DocumentReadiness>,
    /// Normalized source formats included in the result.
    pub source_formats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Revision, provenance, and extracted artifacts for a selected document.
pub struct DocumentDetail {
    /// List-view metadata and current processing state.
    pub summary: DocumentSummary,
    /// Caller-provided identity used for idempotent document mutations.
    pub external_key: Option<String>,
    /// Canonical persisted document lifecycle state when available.
    pub document_state: Option<String>,
    /// Revision currently promoted as the document head.
    pub active_revision: Option<DocumentRevisionSummary>,
    /// Crawl identity when the document originated from a web page.
    pub web_page_provenance: Option<WebPageProvenance>,
    /// Structural segments extracted from the active revision.
    pub prepared_segments: Vec<PreparedSegment>,
    /// Normalized facts extracted from the active revision.
    pub technical_facts: Vec<TechnicalFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Complete state needed to render the document workbench.
pub struct DocumentsSurface {
    /// Counts for the current library and filter scope.
    pub overview: DocumentsOverview,
    /// Filters applied to the document list.
    pub filters: DocumentFilterState,
    /// Documents matching the current filter state.
    pub documents: Vec<DocumentSummary>,
    /// Document selected by the client, if any.
    pub selected_document_id: Option<Uuid>,
    /// Expanded state for the selected document when it could be loaded.
    pub selected_document: Option<DocumentDetail>,
    /// Recent web-ingestion runs associated with the library.
    pub web_runs: Vec<WebIngestRunSummary>,
    /// Non-fatal conditions affecting this workbench snapshot.
    pub warnings: Vec<OperatorWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Lifecycle of a bounded web-ingestion run.
pub enum WebIngestRunState {
    /// Validated and accepted but not yet discovering pages.
    Accepted,
    /// Traversing eligible links and recording page candidates.
    Discovering,
    /// Fetching or materializing discovered candidates.
    Processing,
    /// All eligible candidates reached a successful terminal outcome.
    Completed,
    /// The run completed with some excluded, blocked, or failed candidates.
    CompletedPartial,
    /// A run-level failure prevented successful completion.
    Failed,
    /// Cancellation was requested before normal completion.
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Candidate counters partitioning the work observed by a web-ingestion run.
pub struct WebRunCounts {
    /// Unique page candidates found during traversal.
    pub discovered: i32,
    /// Candidates admitted by boundary and crawl filters.
    pub eligible: i32,
    /// Candidates whose processing reached a terminal outcome.
    pub processed: i32,
    /// Candidates waiting for processing.
    pub queued: i32,
    /// Candidates with active processing attempts.
    pub processing: i32,
    /// Candidates converged with an existing canonical page.
    pub duplicates: i32,
    /// Candidates rejected by configured materialization filters.
    pub excluded: i32,
    /// Candidates rejected by safety or traversal-boundary policy.
    pub blocked: i32,
    /// Candidates ending in processing failure.
    pub failed: i32,
    /// Candidates canceled before a normal terminal outcome.
    pub canceled: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// One typed URL-matching rule used by web-ingestion filters.
pub struct WebIngestPattern {
    /// Matching strategy understood by the web-ingestion policy.
    pub kind: String,
    /// Pattern payload interpreted according to `kind`.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Origin of the rule when it was inferred or imported.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Ordered admission and rejection rules for candidate URLs.
pub struct WebIngestUrlFilter {
    /// Rules that explicitly admit matching URLs.
    pub allow_patterns: Vec<WebIngestPattern>,
    /// Rules that explicitly reject matching URLs.
    pub block_patterns: Vec<WebIngestPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Configuration, progress, and counters for an existing crawl run.
pub struct WebIngestRunSummary {
    /// Stable run identifier.
    pub run_id: Uuid,
    /// Destination library for materialized pages.
    pub library_id: Uuid,
    /// Traversal mode selected when the run was created.
    pub mode: String,
    /// Origin or path boundary applied during discovery.
    pub boundary_policy: String,
    /// Maximum link distance from the seed page.
    pub max_depth: i32,
    /// Maximum unique page candidates admitted by the run.
    pub max_pages: i32,
    /// URL rules applied before fetching a discovered candidate.
    pub crawl_filter: WebIngestUrlFilter,
    /// URL rules applied before persisting a fetched page as a document.
    pub materialization_filter: WebIngestUrlFilter,
    /// Current run lifecycle.
    pub run_state: WebIngestRunState,
    /// Normalized URL from which traversal began.
    pub seed_url: String,
    /// Current candidate-state counters.
    pub counts: WebRunCounts,
    /// Latest observed progress or terminal-state transition.
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Immediate acknowledgement and current state of a web-run mutation.
pub struct WebIngestRunReceipt {
    /// Stable identifier of the affected run.
    pub run_id: Uuid,
    /// Destination library associated with the run.
    pub library_id: Uuid,
    /// Traversal mode accepted for the run.
    pub mode: String,
    /// Run lifecycle observed after applying the mutation.
    pub run_state: WebIngestRunState,
    /// Candidate counters observed after applying the mutation.
    pub counts: WebRunCounts,
    /// Long-running operation that tracks asynchronous execution.
    pub async_operation_id: Option<Uuid>,
    /// Machine-stable reason when the mutation or run failed.
    pub failure_code: Option<String>,
    /// Time at which cancellation was accepted, if requested.
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Actionable condition highlighted on a library dashboard.
pub struct DashboardAttentionItem {
    /// Machine-stable condition identifier.
    pub code: String,
    /// Short operator-facing summary.
    pub title: String,
    /// Explanation of impact or suggested remediation.
    pub detail: String,
    /// Application route where the condition can be inspected.
    pub route_path: String,
    /// Operational severity of the condition.
    pub level: MessageLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Aggregated library state required by the operational dashboard.
pub struct DashboardSurface {
    /// Canonical mutually exclusive document lifecycle and graph-readiness counts.
    pub document_metrics: LibraryDocumentMetrics,
    /// Most recently mutated documents in the library.
    pub recent_documents: Vec<DocumentSummary>,
    /// Most recently active web-ingestion runs.
    pub recent_web_runs: Vec<WebIngestRunSummary>,
    /// Current graph topology and readiness summary.
    pub graph: GraphSurface,
    /// Conditions that may require operator action.
    pub attention: Vec<DashboardAttentionItem>,
    /// Non-fatal conditions affecting dashboard completeness or accuracy.
    pub warnings: Vec<OperatorWarning>,
}
