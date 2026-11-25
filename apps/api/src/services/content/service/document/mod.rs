use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use ironrag_contracts::documents::{DocumentReadiness, DocumentStatus};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::billing::CostRollupFreshness,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentPipelineJob,
        ContentDocumentPipelineState, ContentDocumentSummary, ContentMutation, ContentRevision,
        DOCUMENT_ROLE_ATTACHED_CONTEXT, DOCUMENT_ROLE_ATTACHMENT, WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, PreparedSegmentListItem, TypedTechnicalFact},
    domains::ops::{ASYNC_OP_STATUS_FAILED, MUTATION_KIND_DELETE},
    domains::webhook::{WebhookEvent, document_deleted_event_id},
    infra::knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
    infra::repositories::{
        self, catalog_repository, content_chunk_repository, content_document_head_repository,
        content_document_list_repository::{self, ContentDocumentListRow, DocumentListSortColumn},
        content_document_repository, content_knowledge_projection_repository,
        content_mutation_repository, content_revision_repository, ingest_repository,
        webhook_outbox_repository,
    },
    interfaces::http::router_support::ApiError,
    services::content::source_access::describe_content_source,
    services::ops::service::{DocumentKnowledgeSignals, classify_document_knowledge_signals},
};

use super::{
    ContentService, PrefetchedDocumentSummaryData, ReconcileFailedIngestMutationCommand,
    map_document_pipeline_job, map_document_row, map_knowledge_document_row,
    map_knowledge_revision_readiness, map_knowledge_revision_row, map_mutation_row,
    map_structured_revision_row, map_web_page_provenance_row, segment_excerpt,
};

mod deletion;
mod graph_cleanup;
mod lifecycle;
mod listing;
mod summary;
#[cfg(test)]
mod tests;

use summary::DocumentSummaryInput;

#[derive(Debug, Clone, Default)]
pub(crate) struct DeletedDocumentGraphCleanup {
    pub node_ids: Vec<Uuid>,
    pub edge_ids: Vec<Uuid>,
}

impl DeletedDocumentGraphCleanup {
    #[must_use]
    pub(crate) fn from_targets(targets: Vec<repositories::RuntimeGraphEvidenceTargetRow>) -> Self {
        let mut node_ids = HashSet::new();
        let mut edge_ids = HashSet::new();
        for target in targets {
            match target.target_kind.as_str() {
                "node" => {
                    node_ids.insert(target.target_id);
                }
                "edge" => {
                    edge_ids.insert(target.target_id);
                }
                _ => {}
            }
        }
        Self { node_ids: node_ids.into_iter().collect(), edge_ids: edge_ids.into_iter().collect() }
    }

    #[must_use]
    pub(crate) const fn requires_graph_convergence(&self) -> bool {
        !self.node_ids.is_empty() || !self.edge_ids.is_empty()
    }
}

fn apply_document_hint_to_active_revision(
    summary: &mut ContentDocumentSummary,
    revision_id: Uuid,
    document_hint: Option<String>,
) {
    if let Some(active_revision) =
        summary.active_revision.as_mut().filter(|revision| revision.id == revision_id)
    {
        active_revision.document_hint = document_hint;
    }
}

fn next_document_list_cursor(
    rows: &[ContentDocumentListRow],
    items: &[ContentDocumentListEntry],
    sort: DocumentListSortColumn,
    has_more: bool,
) -> Option<DocumentListCursorValue> {
    // A cost-sorted continuation is a cost-dependent read. Staleness is already
    // excluded upstream: a reconciling rollup answers a pending page with no
    // cursor. An absent per-document cost is a stable "unmeasured" fact, so the
    // keyset's NULL-cost bucket orders it deterministically instead of stopping
    // pagination for every library that holds an unmeasured document.
    if !has_more {
        return None;
    }
    rows.last().map(|row| DocumentListCursorValue {
        created_at: row.created_at,
        document_id: row.id,
        text_value: match sort {
            DocumentListSortColumn::ExternalKey => {
                row.projection_file_name.as_ref().map(|name| name.to_lowercase())
            }
            DocumentListSortColumn::MimeType => row.revision_mime_type.clone(),
            DocumentListSortColumn::DerivedStatus => {
                items.last().map(|item| item.status.as_str().to_string())
            }
            DocumentListSortColumn::Cost => row.cost_total.map(|value| value.to_string()),
            _ => None,
        },
        number_value: match sort {
            DocumentListSortColumn::ByteSize => row.revision_byte_size,
            DocumentListSortColumn::PipelineTime => row.pipeline_time_ms,
            _ => None,
        },
        finished_value: match sort {
            DocumentListSortColumn::FinishedAt => row.finished_at,
            _ => None,
        },
        value_is_null: match sort {
            DocumentListSortColumn::ExternalKey => row.projection_file_name.is_none(),
            DocumentListSortColumn::MimeType => row.revision_mime_type.is_none(),
            DocumentListSortColumn::ByteSize => row.revision_byte_size.is_none(),
            DocumentListSortColumn::Cost => row.cost_total.is_none(),
            DocumentListSortColumn::PipelineTime => row.pipeline_time_ms.is_none(),
            DocumentListSortColumn::FinishedAt => row.finished_at.is_none(),
            _ => false,
        },
    })
}

// ============================================================================
// Document-list types + derivation helpers.
// ============================================================================

#[derive(Debug, Clone)]
pub struct ListDocumentsPageCommand {
    pub library_id: Uuid,
    pub include_deleted: bool,
    pub cursor: Option<content_document_list_repository::DocumentListPageCursor>,
    pub limit: u32,
    pub search: Option<String>,
    /// Intl-normalized variant of `search`, OR'd over the same canonical fields.
    pub normalized_search: Option<String>,
    /// Canonical status buckets a `searchStatus` hint matches.
    pub search_statuses: Vec<String>,
    /// Strictly typed per-column grid filters.
    pub filters: content_document_list_repository::DocumentListFilters,
    /// Typed range hints, OR alternatives to the text/status search.
    pub search_ranges: content_document_list_repository::DocumentListSearchRanges,
    /// Snapshot instant captured once for the whole cursor chain.
    pub snapshot_at: DateTime<Utc>,
    pub sort: DocumentListSortColumn,
    pub sort_desc: bool,
    /// Server-side status filter. Empty = no filter. Values must be one of the
    /// canonical `DocumentStatus` buckets — `canceled`, `empty`, `failed`,
    /// `paused`, `pending_materialization`, `processing`, `queued`, `ready` —
    /// matching the derived `derived_status` column in
    /// `list_document_page_rows`. `DocumentStatus::ALL` is the list of them.
    pub status_filter: Vec<DocumentStatus>,
    /// Server-side document-id filter. `None` = no filter. `Some(ids)`
    /// restricts the page to exactly those documents — including the empty
    /// set, which yields an empty page. It carries both a deep link resolving
    /// one document through the shared list derivation and a filter the server
    /// resolved to a document set of its own, such as `indexing=vector_gap`.
    pub id_filter: Option<Vec<Uuid>>,
    /// Whether to compute the per-status-bucket counts in the same snapshot as
    /// the page. The metadata list still serves real rows while the rollup
    /// reconciles; only the counts are gated on the request asking for them.
    pub include_status_counts: bool,
}

#[derive(Debug, Clone)]
pub struct ContentDocumentListEntry {
    pub id: Uuid,
    pub visibility_intent_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub file_name: Option<String>,
    pub file_type: Option<String>,
    pub file_size: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
    pub document_state: String,
    pub status: DocumentStatus,
    pub readiness: DocumentReadiness,
    pub stage: Option<String>,
    pub progress_percent: Option<i32>,
    pub processing_started_at: Option<DateTime<Utc>>,
    pub processing_finished_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub retryable: bool,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub source_access: Option<crate::domains::content::ContentSourceAccess>,
    /// Persisted connector identity for this document. Comes straight
    /// from `content_document.external_key` and is the key connectors
    /// use to look up "do I already own a doc here?" without paging the
    /// whole library.
    pub external_key: String,
    /// Summed cost across every billable execution attributed to this
    /// document — computed server-side via the per-document LATERAL on
    /// `billing_execution_cost` in the list query, so the frontend never
    /// issues a library-wide `/billing/library-document-costs` fetch.
    /// `None` while the owning rollup reconciles: the amount is not known yet,
    /// which is not the same as zero.
    pub cost_total: Option<rust_decimal::Decimal>,
    pub cost_currency_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DocumentListCursorValue {
    pub created_at: DateTime<Utc>,
    pub document_id: Uuid,
    pub text_value: Option<String>,
    pub number_value: Option<i64>,
    pub finished_value: Option<DateTime<Utc>>,
    pub value_is_null: bool,
}

#[derive(Debug, Clone)]
pub struct ContentDocumentListPageResult {
    pub items: Vec<ContentDocumentListEntry>,
    pub next_cursor: Option<DocumentListCursorValue>,
    pub has_more: bool,
    /// Freshness of the library billing rollup this page was read against,
    /// independent of pricing coverage. `Reconciling` means any row's cost may
    /// lag a committed provider call, so the page suppresses cost fields and no
    /// cost-dependent read may be served from it.
    pub billing_rollup_freshness: CostRollupFreshness,
    /// Whether this request's *query result* had to be withheld because it
    /// reads cost while the rollup reconciles. This is distinct from
    /// `billing_rollup_freshness`: a metadata-only dirty read is
    /// `scope_pending = false` with real rows and a truthful `Reconciling`
    /// marker, while a cost sort / cost filter / text search is
    /// `scope_pending = true` with no items, cursor or total.
    pub scope_pending: bool,
    /// Total of the filtered scope the rows were read from — the status pill
    /// and every other list filter narrow it exactly as they narrow the rows —
    /// observable even when the filter resolves the page to zero rows. `None`
    /// when the read was withheld (`scope_pending`).
    pub total_count: Option<i64>,
    /// Status-bucket counts over the same scope, present only when requested.
    pub status_counts: Option<content_document_list_repository::DocumentListStatusCountsRow>,
    /// Ephemeral identity of the filtered population this page was read from,
    /// computed in the same REPEATABLE READ snapshot as the rows. A continuation
    /// token carries it so a later page can refuse to skip documents whose
    /// status, readable revision or publication commitment changed in between;
    /// a mismatch ends the continuation instead of silently dropping rows. It is
    /// a snapshot identity, not persisted state or a second read model.
    pub scope_fingerprint: String,
}

/// Derives a `ContentDocumentListEntry` from the joined Postgres row alone. The
/// page query already selects the canonical readable revision and that exact
/// revision's channel states, so mapping never performs a second publication or
/// knowledge lookup — a lagging optional projection cannot turn published
/// content into `empty`/`failed`. This is the single shared place where document
/// list status / readiness enums are computed, so there is no drift between the
/// list handler and the library summary aggregator.
fn build_document_list_entry(
    row: &ContentDocumentListRow,
) -> Result<ContentDocumentListEntry, ApiError> {
    let (cost_total, cost_currency_code) = document_costs(row)?;
    let (status, readiness) = document_status_and_readiness(row);
    let source_descriptor = row.readable_revision_id.and_then(|revision_id| {
        let source_kind = row.revision_content_source_kind.as_deref()?;
        Some(describe_content_source(
            row.id,
            Some(revision_id),
            crate::services::content::source_access::ContentSourceInputs {
                content_source_kind: source_kind,
                source_uri: row.revision_source_uri.as_deref(),
                storage_key: row.revision_storage_key.as_deref(),
                title: row.revision_title.as_deref(),
                external_key: &row.external_key,
            },
        ))
    });
    // Use the file name selected by the page query itself. This keeps the
    // rendered value and the keyset cursor on one database snapshot even if
    // the denormalized knowledge projection changes while the page is mapped.
    let file_name = row.projection_file_name.clone();
    let file_type = row.revision_mime_type.clone();
    let file_size = row.revision_byte_size;
    let processing_finished_at = (status != DocumentStatus::Processing)
        .then_some(row.job_completed_at.or(row.attempt_finished_at))
        .flatten();

    Ok(ContentDocumentListEntry {
        id: row.id,
        visibility_intent_id: row.visibility_intent_id,
        readable_revision_id: row.readable_revision_id,
        library_id: row.library_id,
        workspace_id: row.workspace_id,
        file_name,
        file_type,
        file_size,
        uploaded_at: row.created_at,
        document_state: row.document_state.clone(),
        external_key: row.external_key.clone(),
        status,
        readiness,
        stage: row.attempt_current_stage.clone(),
        progress_percent: derive_document_list_progress_percent(
            status,
            row.attempt_progress_percent.unwrap_or_default(),
        ),
        processing_started_at: row.attempt_started_at,
        processing_finished_at,
        failure_code: row
            .attempt_failure_code
            .clone()
            .or_else(|| row.mutation_failure_code.clone()),
        failure_message: row.attempt_failure_message.clone(),
        retryable: row.attempt_retryable.unwrap_or(false),
        source_kind: row.revision_content_source_kind.clone(),
        source_uri: row.revision_source_uri.clone(),
        document_hint: row.revision_document_hint.clone(),
        source_access: source_descriptor.and_then(|descriptor| descriptor.access),
        cost_total,
        cost_currency_code,
    })
}

fn document_costs(
    row: &ContentDocumentListRow,
) -> Result<(Option<rust_decimal::Decimal>, Option<String>), ApiError> {
    // The list no longer fails closed on a dirty rollup. A reconciling rollup
    // suppresses this row's cost so the page never orders or renders a stale
    // per-document total as exact; the page-level freshness marker names the
    // state. A terminal mixed-currency rollup keeps its 409.
    if row.billing_rollup_terminal_error_code.is_some() {
        crate::services::ops::billing::ensure_billing_rollup_readable(
            "library",
            row.library_id,
            false,
            row.billing_rollup_terminal_error_code.as_deref(),
        )?;
    }
    if row.billing_rollup_dirty {
        return Ok((None, None));
    }
    // A document whose derived costs span currencies is still a dimensionally
    // invalid sum and fails the page closed.
    if row.billing_cost_mixed_currency {
        return Err(ApiError::Conflict(format!(
            "document {} has billing costs in multiple currencies; request per-currency charges",
            row.id
        )));
    }
    // No measured price is a truthful state, not an error: the row keeps both
    // cost fields absent so the grid renders no amount and the cost filter
    // excludes it instead of treating the document as free.
    match (&row.cost_total, &row.cost_currency_code) {
        (Some(total), Some(currency_code)) => Ok((Some(*total), Some(currency_code.clone()))),
        _ => Ok((None, None)),
    }
}

fn document_status_and_readiness(
    row: &ContentDocumentListRow,
) -> (DocumentStatus, DocumentReadiness) {
    let signals = document_knowledge_signals(row);
    let readiness = document_readiness(row, signals);
    (document_status(row), readiness)
}

fn document_knowledge_signals(row: &ContentDocumentListRow) -> DocumentKnowledgeSignals<'static> {
    // Readable presence is the canonical publication fact: `readable_revision_id`
    // is selected through `content_current_revision`, which already fences
    // `text_state = 'text_readable'`. Channel states from the same row only add
    // optional capability, so an absent or lagging knowledge projection can never
    // remove readable content.
    let revision_text_ready = row.readable_revision_id.is_some();
    let revision_vector_ready = row.knowledge_vector_state.as_deref() == Some("ready");
    let revision_graph_ready = row.knowledge_graph_state.as_deref() == Some("ready");
    let revision_graph_degraded = row.knowledge_graph_state.as_deref() == Some("graph_degraded");
    let graph_failed = row.knowledge_graph_state.as_deref() == Some("failed");
    let mutation_failed = row.mutation_state.as_deref().is_some_and(|state| state == "failed");
    let job_failed = job_failed_hides_readable_content(row);

    DocumentKnowledgeSignals {
        processing_active: mutation_is_inflight(row) || job_is_inflight(row),
        hard_failure: mutation_failed || job_failed,
        canceled_terminal: mutation_is_canceled(row) || job_is_canceled(row),
        revision_text_ready,
        revision_vector_ready,
        revision_graph_ready,
        revision_graph_degraded,
        preparation_ready: false,
        preparation_failed: false,
        graph_failed,
        observed_preparation_state: None,
        block_count: None,
        typed_fact_count: None,
    }
}

fn document_readiness(
    row: &ContentDocumentListRow,
    signals: DocumentKnowledgeSignals<'_>,
) -> DocumentReadiness {
    if row.document_state != "deleted" && row.deleted_at.is_none() {
        classify_document_knowledge_signals(signals).readiness_kind
    } else {
        DocumentReadiness::Processing
    }
}

/// Per-row mirror of `DERIVED_STATUS_CASE_SQL` in
/// `infra::repositories::content_document_list_repository`. Operational status is
/// derived from the same Postgres-visible signals the SQL filter uses — mutation,
/// latest job kind/state, current readable revision and the publication
/// commitment — so the filter, the pill and the payload can never name different
/// buckets, and enhancement readiness never reclassifies readable content.
fn document_status(row: &ContentDocumentListRow) -> DocumentStatus {
    if mutation_has_failed(row) || job_failed_hides_readable_content(row) {
        return DocumentStatus::Failed;
    }
    if row.job_queue_state.as_deref() == Some("leased") {
        return DocumentStatus::Processing;
    }
    if row.readable_revision_id.is_some() {
        return DocumentStatus::Ready;
    }
    if mutation_is_canceled(row) || job_is_canceled(row) {
        return DocumentStatus::Canceled;
    }
    if row.job_queue_state.as_deref() == Some("queued") {
        return DocumentStatus::Queued;
    }
    if mutation_is_inflight(row) {
        return DocumentStatus::Processing;
    }
    if row.job_queue_state.as_deref() == Some("paused") {
        return DocumentStatus::Paused;
    }
    if row.job_queue_state.as_deref() == Some("completed") {
        return DocumentStatus::Failed;
    }
    if document_pending_materialization(row) {
        return DocumentStatus::PendingMaterialization;
    }
    DocumentStatus::Empty
}

/// Canonical suppression fact shared with
/// `ContentDocumentReadSnapshot.suppressed` / `unresolved_head_suppression!()`:
/// the head holds an accepted publication commitment and no current readable
/// revision exists. It is not work in flight, so it must not read as `queued`
/// or `processing`; it is also not the content-free admission `empty` names.
fn document_pending_materialization(row: &ContentDocumentListRow) -> bool {
    row.visibility_intent_id.is_some() && row.readable_revision_id.is_none()
}

/// Mirror of the SQL enrichment branch: a failed `graph_refresh`/`reembed` job
/// only enriches material an earlier revision already published, so it must stay
/// `ready` while that readable revision is still canonical. Every other failed
/// job hides readable content and stays `failed`.
fn job_failed_hides_readable_content(row: &ContentDocumentListRow) -> bool {
    if !job_has_failed(row) {
        return false;
    }
    let enrichment_only =
        content_document_list_repository::is_document_enrichment_job_kind(row.job_kind.as_deref());
    !(enrichment_only && row.readable_revision_id.is_some())
}

fn mutation_has_failed(row: &ContentDocumentListRow) -> bool {
    row.mutation_state.as_deref().is_some_and(|state| state == "failed")
}

fn job_has_failed(row: &ContentDocumentListRow) -> bool {
    row.job_queue_state.as_deref() == Some("failed")
}

fn mutation_is_canceled(row: &ContentDocumentListRow) -> bool {
    row.mutation_state.as_deref() == Some("canceled")
}

fn job_is_canceled(row: &ContentDocumentListRow) -> bool {
    row.job_queue_state.as_deref() == Some("canceled")
}

fn mutation_is_inflight(row: &ContentDocumentListRow) -> bool {
    row.mutation_state.as_deref().is_some_and(|state| matches!(state, "accepted" | "running"))
}

fn job_is_inflight(row: &ContentDocumentListRow) -> bool {
    row.job_queue_state.as_deref().is_some_and(|state| matches!(state, "queued" | "leased"))
}

fn derive_document_list_progress_percent(
    status: DocumentStatus,
    attempt_progress_percent: i32,
) -> Option<i32> {
    match status {
        DocumentStatus::Ready => Some(100),
        DocumentStatus::Queued | DocumentStatus::Empty => Some(0),
        // A held job reports where its last attempt stopped; a pending
        // publication commitment has no attempt of its own, so it reports no
        // progress instead of a fabricated 0 %.
        DocumentStatus::PendingMaterialization => None,
        DocumentStatus::Processing
        | DocumentStatus::Failed
        | DocumentStatus::Canceled
        | DocumentStatus::Paused => Some(attempt_progress_percent.clamp(0, 99)),
    }
}
