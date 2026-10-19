use std::future::Future;

use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

/// Shared CASE expression that derives the six status buckets the
/// documents surface exposes (`canceled` / `empty` / `failed` /
/// `processing` / `queued` / `ready`) from Postgres-only signals. One
/// source of truth so the list page, the status-count aggregate, and
/// every ad-hoc caller stay aligned.
///
/// Priority (top row wins):
/// 1. Mutation is terminally failed → `failed`.
///    The head itself is broken; the operator must see this.
/// 2. A background graph/vector refresh failed over an already readable
///    revision → `ready`. The optional enhancement failed, not the material.
/// 3. Latest `ingest_job` is `failed` → `failed`.
/// 4. Latest `ingest_job` is `leased` → `processing`. A worker is
///    actively running this document; surface it regardless of
///    whether a previous readable revision exists, so the operator
///    can see the pipeline moving even during bulk re-ingest.
/// 5. `content_current_revision.revision_id` is set → `ready`.
///    The document has a usable revision the user can consume
///    right now. `ready` wins over `canceled` / `queued`:
///    a canceled or queued re-ingest over a still-readable
///    document should not hide it from the ready bucket. Otherwise
///    canceled fan-out jobs can dominate the pick during bulk
///    re-ingest.
/// 6. Latest `ingest_job` or mutation is `canceled` → `canceled` (no
///    readable, work was canceled before finishing).
/// 7. Latest `ingest_job` is `queued` → `queued` (new document
///    waiting for its first ingest; no readable yet).
/// 8. Mutation state is `accepted` / `running` → `processing`. Checked
///    before the `paused` branch below: pausing an `ingest_job`
///    (`pause_ingest_job`) never touches `content_mutation`, so a job
///    paused mid-run still has its mutation `running` — that document
///    is actively being worked, not merely held.
/// 9. Latest `ingest_job` is `paused` → `queued`. Held, not dropped:
///    the baseline schema's own `idx_ingest_job_active_queue_rank`
///    groups `queued` / `leased` / `paused` as one active-queue class.
///    It does not resume on its own — `resume_ingest_job` and
///    `retry_or_requeue_ingest_job` are reachable only through
///    explicit, permission-gated operator endpoints — but it is held
///    work waiting on that operator action, not the "nothing will
///    ever move without new content" case `empty` names below.
/// 10. Latest `ingest_job` is `completed` but no readable → `failed`
///     (post-completion head update did not land; surface the anomaly).
/// 11. Everything else → `empty`. By construction this is only reached
///     with no `ingest_job` row at all (every `ingest_job.queue_state`
///     value is matched above) and a mutation that is neither
///     actively working (`accepted` / `running`) nor terminally
///     failed / canceled — i.e. `applied` (metadata-only admission,
///     `settle_metadata_only_admission`, IRG-107), `conflicted`, or
///     absent (`latest_mutation_id is null`, pre-2026-07-15 legacy
///     rows predating the atomic-admission head write). None of these
///     is "waiting for a worker": nothing will move without a new
///     mutation supplying content.
///
/// Requires the hosting query to expose `ij.queue_state`,
/// `m.mutation_state`, and `current_revision.revision_id` under exactly
/// those aliases (both current callers do). `ij.queue_state` must
/// be picked from this document's newest mutation, with state
/// priority only as a retry tie-breaker inside that mutation — see
/// `list_document_page_rows` for the reference implementation.
///
/// This ordering must stay in lockstep with `document_status()` in
/// `services/content/service/document/mod.rs`, which computes the
/// same six buckets per-row from Rust-side signals for the API
/// response. Both fall through the same `accepted`/`running` check
/// before the `paused` check for the reason in step 8.
pub(crate) const DERIVED_STATUS_CASE_SQL: &str = "case
    when m.mutation_state = 'failed' then 'failed'
    when ij.queue_state = 'failed'
         and ij.job_kind in ('graph_refresh','reembed')
          and current_revision.revision_id is not null then 'ready'
    when ij.queue_state = 'failed' then 'failed'
    when ij.queue_state = 'leased' then 'processing'
    when current_revision.revision_id is not null then 'ready'
    when ij.queue_state = 'canceled' then 'canceled'
    when m.mutation_state = 'canceled' then 'canceled'
    when ij.queue_state = 'queued' then 'queued'
    when m.mutation_state in ('accepted','running') then 'processing'
    when ij.queue_state = 'paused' then 'queued'
    when ij.queue_state = 'completed' then 'failed'
    else 'empty'
end";

/// Publication supplies metadata, never operational document membership.
/// Scope all callers of the shared status decision to the same canonical view.
pub(crate) const CURRENT_DOCUMENT_REVISION_JOIN_SQL: &str =
    "left join content_current_revision current_revision
                on current_revision.document_id = d.id
               and current_revision.workspace_id = d.workspace_id
               and current_revision.library_id = d.library_id";

fn literal_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('%');
    for character in value.to_lowercase().chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}

/// Selects the newest ingest job for one document, preferring an active retry
/// over older terminal attempts from the same mutation. Every document-list
/// query must use this exact ordering or status buckets and graph coverage can
/// disagree about the same document. Also reused by the ops failed-attempt query
/// so its notice fires only for documents the list would still show as `failed`.
pub(crate) const LATEST_DOCUMENT_INGEST_JOB_SQL: &str = "select
                    ij_inner.id,
                    ij_inner.job_kind,
                    ij_inner.queue_state,
                    ij_inner.queued_at,
                    ij_inner.completed_at
                from ingest_job ij_inner
                 left join content_mutation m_inner on m_inner.id = ij_inner.mutation_id
                 where ij_inner.knowledge_document_id = d.id
                   and (ij_inner.mutation_id is null or m_inner.id is not null)
                 order by coalesce(m_inner.requested_at, ij_inner.queued_at) desc,
                    case ij_inner.queue_state::text
                        when 'leased' then 1
                        when 'failed' then 2
                        when 'canceled' then 3
                        when 'queued' then 4
                        when 'completed' then 5
                        else 6
                    end,
                    ij_inner.queued_at desc
                limit 1";
// ============================================================================
// Primary slim-list query for /v1/content/documents (list_documents_page).
// ============================================================================

/// One row of the paginated document-list query. Joins the minimum set of
/// tables required to render the document list card server-side (status,
/// readiness, nullable publication name, source access) without any per-document
/// round-trips. Knowledge-plane readiness signals (`knowledge_revision.text_state`
/// etc.) are merged in by the caller.
#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub projection_file_name: Option<String>,
    pub document_state: String,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,

    // head pointers — may be absent while the document is still in flight
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,

    // active revision metadata (Postgres copy)
    pub revision_title: Option<String>,
    pub revision_mime_type: Option<String>,
    pub revision_byte_size: Option<i64>,
    pub revision_source_uri: Option<String>,
    pub revision_document_hint: Option<String>,
    pub revision_content_source_kind: Option<String>,
    pub revision_storage_key: Option<String>,

    // latest mutation
    pub mutation_id: Option<Uuid>,
    pub mutation_state: Option<String>,
    pub mutation_failure_code: Option<String>,
    pub mutation_requested_at: Option<DateTime<Utc>>,

    // latest ingest job (only one per mutation)
    pub job_id: Option<Uuid>,
    pub job_queue_state: Option<String>,
    pub job_queued_at: Option<DateTime<Utc>>,
    pub job_completed_at: Option<DateTime<Utc>>,

    // latest attempt on that job
    pub attempt_current_stage: Option<String>,
    pub attempt_started_at: Option<DateTime<Utc>>,
    pub attempt_finished_at: Option<DateTime<Utc>>,
    pub attempt_failure_code: Option<String>,
    pub attempt_retryable: Option<bool>,
    pub attempt_heartbeat_at: Option<DateTime<Utc>>,
    pub attempt_failure_message: Option<String>,
    pub attempt_progress_percent: Option<i32>,

    // Derived ordering projections, hoisted into the shared base CTE so the
    // list can sort/filter on them without a second pass. Kept identical to
    // the values the frontend reads off `processingStartedAt` /
    // `processingFinishedAt`.
    /// `coalesce(latest ingest_job.completed_at, latest attempt.finished_at)`.
    /// `NULL` while no attempt has finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// Worker-holding duration in milliseconds. Mirrors the frontend
    /// `getDocumentProcessingDurationMs`: for a document whose worker is still
    /// holding the job (`processing` / `queued`) the elapsed time is measured
    /// against the request snapshot, otherwise against the terminal finish.
    /// `NULL` when no attempt ever started.
    pub pipeline_time_ms: Option<i64>,

    // per-document billing rollup — summed across every execution
    // attributed to this document (ingest_attempt + graph_extraction_attempt).
    // Surfaced on the primary list response so the frontend never has
    // to issue a library-wide `/billing/library-document-costs` fetch to
    // fill in the cost column.
    /// `None` is a fail-closed mixed-currency sentinel. The service rejects
    /// the complete page instead of presenting a dimensionally invalid sum.
    pub cost_total: Option<rust_decimal::Decimal>,
    pub cost_currency_code: Option<String>,
    /// Scope health is selected in the same Postgres statement snapshot as
    /// the per-document cost, preventing a clean-check/stale-total race.
    pub billing_rollup_dirty: bool,
    pub billing_rollup_terminal_error_code: Option<String>,
}

/// Ordering key for the document-list keyset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentListSortColumn {
    /// Default: upload time, matching the frontend "Uploaded" column.
    CreatedAt,
    /// Lexicographic on the exact-scoped current publication name shown
    /// by the UI. The legacy variant name is internal only; external keys stay
    /// separate document identities and must not affect visible ordering.
    ExternalKey,
    /// Sort by `content_revision.mime_type` (file type column).
    MimeType,
    /// Sort by `content_revision.byte_size` (file size column).
    ByteSize,
    /// Sort by `derived_status` — same CASE expression used for the
    /// status pills, so operators can group ready/failed/processing.
    DerivedStatus,
    /// Sort by the per-document cost rollup. A dirty rollup or a
    /// mixed-currency sum is `NULL` and therefore lands in the NULL bucket,
    /// never a fabricated `0`.
    Cost,
    /// Sort by the worker-holding duration (`pipeline_time_ms`).
    PipelineTime,
    /// Sort by the terminal finish timestamp (`finished_at`).
    FinishedAt,
}

/// Facet columns the documents data grid can request distinct values for.
/// Identifiers are the exact column ids `DocumentListSortColumn` and the HTTP
/// sort key use, so one grid column maps to one canonical key on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFacetColumn {
    UploadedAt,
    FileName,
    FileType,
    FileSize,
    Status,
    Cost,
    PipelineTime,
    FinishedAt,
}

/// One typed grid-column filter. `present` is false when the caller sent no
/// constraint for the column (absent key, whole-key `null`, or an empty array).
/// `include_null` matches the column's own NULL bucket, and `values` are the
/// finite typed values OR'd together: a column present with only
/// `include_null` selects exactly the NULL bucket, and one present with only
/// `values` selects the non-NULL bucket.
#[derive(Debug, Clone)]
pub struct FilterValues<T> {
    pub present: bool,
    pub include_null: bool,
    pub values: Vec<T>,
}

impl<T> Default for FilterValues<T> {
    fn default() -> Self {
        Self { present: false, include_null: false, values: Vec::new() }
    }
}

/// Strictly typed per-column grid filters. Empty/absent columns impose no
/// constraint; a column with several values is OR within, AND across columns.
#[derive(Debug, Clone, Default)]
pub struct DocumentListFilters {
    pub file_name: FilterValues<String>,
    pub file_type: FilterValues<String>,
    pub file_size: FilterValues<i64>,
    pub uploaded_at: FilterValues<DateTime<Utc>>,
    pub status: FilterValues<String>,
    pub cost: FilterValues<rust_decimal::Decimal>,
    pub pipeline_time: FilterValues<i64>,
    pub finished_at: FilterValues<DateTime<Utc>>,
}

/// Typed extra search hints. Each present range is an OR alternative to the
/// literal / normalized / status search terms (never an AND with them): a row
/// matches when any active hint matches. Bounds are half-open `[min, max)`.
/// These are search hints, not grid-column value filters.
#[derive(Debug, Clone, Default)]
pub struct DocumentListSearchRanges {
    /// `revision.byte_size` — the same projection the Size column displays.
    pub file_size: Option<(i64, i64)>,
    /// `pipeline_time_ms` — the same projection the Time column displays.
    pub pipeline_time: Option<(i64, i64)>,
    /// Matches the upload instant OR the displayed finish instant.
    pub date: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

#[derive(Debug, Clone)]
pub struct DocumentListPageCursor {
    pub created_at: DateTime<Utc>,
    pub document_id: Uuid,
    pub text_value: Option<String>,
    pub number_value: Option<i64>,
    /// Primary timestamp value for the `finished_at` sort — carried
    /// separately from `created_at`, which is only the tie-breaker.
    pub finished_value: Option<DateTime<Utc>>,
    pub value_is_null: bool,
}

/// One page worth of document-list rows. `cursor_*` fields describe the
/// `(created_at, id)` tuple of the last row returned, allowing the caller to
/// construct an opaque continuation token without re-reading the result.
pub struct ContentDocumentListPage {
    pub rows: Vec<ContentDocumentListRow>,
    pub has_more: bool,
}

/// Library-scoped filters and keyset ordering for one document-list page.
#[derive(Debug, Clone)]
pub struct DocumentListPageQuery<'a> {
    pub library_id: Uuid,
    pub include_deleted: bool,
    pub cursor: Option<&'a DocumentListPageCursor>,
    pub limit: u32,
    pub search: Option<&'a str>,
    /// Additional search pattern produced by the caller's Intl-aware
    /// normalization; OR'd alongside `search` over the same canonical fields.
    pub normalized_search: Option<&'a str>,
    /// Canonical status buckets a `searchStatus` hint matches. OR'd alongside
    /// the text search over the canonical fields.
    pub search_statuses: &'a [String],
    /// Strictly typed per-column grid filters.
    pub filters: &'a DocumentListFilters,
    /// Typed range hints, OR alternatives to the text/status search.
    pub search_ranges: &'a DocumentListSearchRanges,
    /// Snapshot instant captured once for the whole cursor chain so sorting a
    /// worker-holding duration is deterministic across pages.
    pub snapshot_at: DateTime<Utc>,
    pub sort: DocumentListSortColumn,
    pub sort_desc: bool,
    pub status_filter: &'a [String],
    pub id_filter: Option<&'a [Uuid]>,
}

/// Library-scoped filters and aggregate scope for the status-count pass.
#[derive(Debug, Clone)]
pub struct DocumentListCountQuery<'a> {
    pub library_id: Uuid,
    pub include_deleted: bool,
    pub search: Option<&'a str>,
    pub normalized_search: Option<&'a str>,
    pub search_statuses: &'a [String],
    pub filters: &'a DocumentListFilters,
    /// Typed range hints, OR alternatives to the text/status search.
    pub search_ranges: &'a DocumentListSearchRanges,
    /// The list page's status pill, applied to the counts too so the badges
    /// describe exactly the set the page is showing.
    pub status_filter: &'a [String],
    /// The resolved id filter (deep link / indexing invariant) the page uses.
    pub id_filter: Option<&'a [Uuid]>,
    pub snapshot_at: DateTime<Utc>,
}

/// Library-scoped request for one page of distinct facet values.
#[derive(Debug)]
pub struct DocumentListFacetQuery<'a> {
    pub library_id: Uuid,
    pub include_deleted: bool,
    pub column: DocumentFacetColumn,
    pub search: Option<&'a str>,
    pub normalized_search: Option<&'a str>,
    pub search_statuses: &'a [String],
    /// Extra substring filter applied to the facet value itself.
    pub facet_search: Option<&'a str>,
    /// Exact canonical values a facet-status-value hint matches (Status only).
    pub facet_search_statuses: &'a [String],
    /// A single typed facet-value range `(min, max)` bound as text and cast in
    /// SQL to the selected column's type (Size/Duration `::bigint`,
    /// Uploaded/Finished `::timestamptz`). `None` = no facet range hint.
    pub facet_search_range: Option<(String, String)>,
    pub filters: &'a DocumentListFilters,
    /// Typed range hints, OR alternatives to the text/status search.
    pub search_ranges: &'a DocumentListSearchRanges,
    /// The list page's status pill. It is not the grid column's own filter, so
    /// it stays applied even when the facet self-excludes `filters.status`.
    pub status_filter: &'a [String],
    pub snapshot_at: DateTime<Utc>,
    pub cursor: Option<&'a DocumentFacetCursorValue>,
    pub limit: u32,
    pub id_filter: Option<&'a [Uuid]>,
}

/// Typed keyset value for a facet page. The variant is fixed by the facet
/// column, so a cursor from another column is a decode mismatch the HTTP layer
/// rejects.
#[derive(Debug, Clone)]
pub enum DocumentFacetCursorValue {
    Text(String),
    Number(i64),
    Timestamp(DateTime<Utc>),
    Decimal(rust_decimal::Decimal),
    Null,
}

/// One distinct facet value with its row count. Exactly one of the typed
/// value slots is populated (or none for the NULL group).
#[derive(Debug, Clone, Default, FromRow)]
pub struct DocumentFacetRow {
    pub value_text: Option<String>,
    pub value_number: Option<i64>,
    pub value_timestamp: Option<DateTime<Utc>>,
    pub value_decimal: Option<rust_decimal::Decimal>,
    pub facet_count: i64,
}

/// One page of distinct facet values.
pub struct DocumentFacetPage {
    pub rows: Vec<DocumentFacetRow>,
    pub has_more: bool,
}

/// Canonical LIKE pattern for a user search: trimmed, lowercased and with
/// `\`, `%`, `_` escaped so the term matches literally.
fn normalize_search_pattern(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(literal_like_pattern)
}

/// Every canonical data value a literal search term is matched against:
/// display name, external key, MIME type, derived status, byte size, cost,
/// pipeline duration and both ISO timestamps. All non-text values are cast to
/// canonical text and NULLs collapse to `''` so they never match a term.
const DOCUMENT_LIST_SEARCH_FIELD_EXPRESSIONS: [&str; 9] = [
    "lower(j.external_key)",
    "lower(j.projection_file_name)",
    "lower(coalesce(j.revision_mime_type, ''))",
    "lower(j.derived_status)",
    "coalesce(j.revision_byte_size::text, '')",
    "coalesce(j.cost_total::text, '')",
    "coalesce(j.pipeline_time_ms::text, '')",
    "to_char(j.created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    "coalesce(to_char(j.finished_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), '')",
];

fn document_list_search_fields_sql(param: &str) -> String {
    DOCUMENT_LIST_SEARCH_FIELD_EXPRESSIONS
        .iter()
        .map(|expression| format!("{expression} like {param} escape '\\'"))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The eight strictly-typed grid-column filter fragments, applied over the `j`
/// alias of the shared base CTE. Placeholders are fixed at $6..$29: per column,
/// `present` (bool), `values` (typed array) and `include_null` (bool). A
/// fragment whose `present` is false never narrows — that is what lets the
/// facet query disable exactly the selected column while keeping the statement
/// parameter-count stable.
const DOCUMENT_LIST_FILTERS_SQL: &str = "
              and (not $6::bool or j.projection_file_name = any($7::text[]) or ($8::bool and j.projection_file_name is null))
              and (not $9::bool or j.revision_mime_type = any($10::text[]) or ($11::bool and j.revision_mime_type is null))
              and (not $12::bool or j.revision_byte_size = any($13::bigint[]) or ($14::bool and j.revision_byte_size is null))
              and (not $15::bool or j.created_at = any($16::timestamptz[]) or ($17::bool and j.created_at is null))
              and (not $18::bool or j.derived_status = any($19::text[]) or ($20::bool and j.derived_status is null))
              and (not $21::bool or j.cost_total = any($22::numeric[]) or ($23::bool and j.cost_total is null))
              and (not $24::bool or j.pipeline_time_ms = any($25::bigint[]) or ($26::bool and j.pipeline_time_ms is null))
              and (not $27::bool or j.finished_at = any($28::timestamptz[]) or ($29::bool and j.finished_at is null))";

/// Library-scoped billing rollup health, shared by the document-list base CTE
/// and the standalone readability probe so both ask the database the same two
/// questions ($1 = library id). The list path fails closed on this health, and
/// the cost facet must fail closed on exactly the same rows.
const DOCUMENT_LIST_BILLING_HEALTH_CTE_SQL: &str = "billing_health as materialized (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.applied_generation < rollup_state.dirty_generation
                ) as rollup_dirty,
                (
                    select rollup_state.terminal_error_code
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.terminal_error_code is not null
                    order by rollup_state.owning_execution_id
                    limit 1
                ) as terminal_error_code
        )";

/// The single shared base CTE for the list, the status counts and the facet
/// aggregate: one projection, one scope predicate, one filter/search set.
///
/// Parameter layout, identical for every consumer:
/// `$1` library, `$2` include_deleted, `$3` search, `$4` normalized search,
/// `$5` searchStatus buckets, `$6..$29` column filters, `$30` snapshot,
/// `$31` id-filter-active, `$32` id filter, `$33/$34` file-size `[min,max)`,
/// `$35/$36` pipeline-time `[min,max)`, `$37/$38` date `[min,max)` (all NULL
/// when the caller sent no such range).
fn document_list_base_cte_sql() -> String {
    let search_sql = format!(
        "and (
                ($3::text is null and $4::text is null and cardinality($5::text[]) = 0
                 and $33::bigint is null and $35::bigint is null and $37::timestamptz is null)
                or ($3::text is not null and ({}))
                or ($4::text is not null and ({}))
                or (cardinality($5::text[]) > 0 and lower(j.derived_status) = any($5))
                or ($33::bigint is not null
                    and j.revision_byte_size >= $33 and j.revision_byte_size < $34)
                or ($35::bigint is not null
                    and j.pipeline_time_ms >= $35 and j.pipeline_time_ms < $36)
                or ($37::timestamptz is not null and (
                      (j.created_at >= $37 and j.created_at < $38)
                      or (j.finished_at >= $37 and j.finished_at < $38)
                   ))
              )",
        document_list_search_fields_sql("$3"),
        document_list_search_fields_sql("$4"),
    );
    format!(
        "with {DOCUMENT_LIST_BILLING_HEALTH_CTE_SQL}, joined as (
            select
                d.id,
                d.workspace_id,
                d.library_id,
                d.external_key,
                current_revision.source_file_name as projection_file_name,
                d.document_state::text as document_state,
                d.created_at,
                d.deleted_at,
                current_revision.revision_id as active_revision_id,
                current_revision.revision_id as readable_revision_id,
                h.latest_mutation_id,
                m.mutation_state::text as mutation_state,
                m.failure_code as mutation_failure_code,
                m.requested_at as mutation_requested_at,
                m.id as mutation_id,
                r.title as revision_title,
                r.mime_type as revision_mime_type,
                r.byte_size as revision_byte_size,
                r.source_uri as revision_source_uri,
                r.document_hint as revision_document_hint,
                r.content_source_kind::text as revision_content_source_kind,
                r.storage_key as revision_storage_key,
                ij.id as job_id,
                ij.queue_state::text as job_queue_state,
                ij.queued_at as job_queued_at,
                ij.completed_at as job_completed_at,
                a.current_stage as attempt_current_stage,
                a.started_at as attempt_started_at,
                a.finished_at as attempt_finished_at,
                a.failure_code as attempt_failure_code,
                a.retryable as attempt_retryable,
                a.heartbeat_at as attempt_heartbeat_at,
                a.failure_message as attempt_failure_message,
                a.progress_percent as attempt_progress_percent,
                c.cost_total,
                c.cost_currency_code,
                billing_health.rollup_dirty as billing_rollup_dirty,
                billing_health.terminal_error_code as billing_rollup_terminal_error_code,
                case
                    when {DERIVED_STATUS_CASE_SQL} = 'processing' then null
                    else coalesce(ij.completed_at, a.finished_at)
                end as finished_at,
                case
                    when a.started_at is null then null
                    when {DERIVED_STATUS_CASE_SQL} = 'processing'
                        then greatest(0, floor(extract(epoch from ($30::timestamptz - a.started_at)) * 1000))::bigint
                    when {DERIVED_STATUS_CASE_SQL} = 'queued'
                        then greatest(0, floor(extract(epoch from (coalesce(ij.completed_at, a.finished_at, $30::timestamptz) - a.started_at)) * 1000))::bigint
                    when coalesce(ij.completed_at, a.finished_at) is null then null
                    else greatest(0, floor(extract(epoch from (coalesce(ij.completed_at, a.finished_at) - a.started_at)) * 1000))::bigint
                end as pipeline_time_ms,
                {DERIVED_STATUS_CASE_SQL} as derived_status
            from content_document d
            {CURRENT_DOCUMENT_REVISION_JOIN_SQL}
            left join content_document_head h on h.document_id = d.id
            left join content_revision r
                on r.id = current_revision.revision_id
            left join content_mutation m on m.id = h.latest_mutation_id
            left join lateral (
                {LATEST_DOCUMENT_INGEST_JOB_SQL}
            ) ij on true
            left join lateral (
                select ia.*
                from ingest_attempt ia
                where ia.job_id = ij.id
                order by ia.attempt_number desc
                limit 1
            ) a on true
            cross join billing_health
            left join lateral (
                select
                    case
                        when count(distinct bec.currency_code) <= 1
                        then coalesce(sum(bec.total_cost), 0)
                    end as cost_total,
                    case
                        when count(distinct bec.currency_code) <= 1
                        then coalesce(min(bec.currency_code), 'USD')
                    end as cost_currency_code
                from billing_execution_cost bec
                where bec.library_id = d.library_id
                  and bec.knowledge_document_id = d.id
            ) c on not billing_health.rollup_dirty
                  and billing_health.terminal_error_code is null
            where d.library_id = $1
              and ($2::bool or (d.document_state = 'active' and d.deleted_at is null))
        ), base as (
            select * from joined j
            where true
              {search_sql}
              {DOCUMENT_LIST_FILTERS_SQL}
              and (not $31::bool or j.id = any($32::uuid[]))
        )",
    )
}

/// Binds the fixed `$1..$38` base-CTE parameters in the one canonical order
/// every consumer shares. `exclude` disables exactly the selected facet
/// column's filter by binding its `present` flag false.
macro_rules! bind_document_list_base {
    ($query:expr, $library_id:expr, $include_deleted:expr, $search:expr, $normalized_search:expr,
     $search_statuses:expr, $filters:expr, $search_ranges:expr, $snapshot_at:expr, $id_filter:expr,
     $exclude:expr) => {{
        let filters = $filters;
        let excluded = $exclude;
        let ranges = $search_ranges;
        $query
            .bind($library_id)
            .bind($include_deleted)
            .bind($search)
            .bind($normalized_search)
            .bind($search_statuses)
            .bind(filters.file_name.present && excluded != Some(DocumentFacetColumn::FileName))
            .bind(filters.file_name.values.as_slice())
            .bind(filters.file_name.include_null)
            .bind(filters.file_type.present && excluded != Some(DocumentFacetColumn::FileType))
            .bind(filters.file_type.values.as_slice())
            .bind(filters.file_type.include_null)
            .bind(filters.file_size.present && excluded != Some(DocumentFacetColumn::FileSize))
            .bind(filters.file_size.values.as_slice())
            .bind(filters.file_size.include_null)
            .bind(filters.uploaded_at.present && excluded != Some(DocumentFacetColumn::UploadedAt))
            .bind(filters.uploaded_at.values.as_slice())
            .bind(filters.uploaded_at.include_null)
            .bind(filters.status.present && excluded != Some(DocumentFacetColumn::Status))
            .bind(filters.status.values.as_slice())
            .bind(filters.status.include_null)
            .bind(filters.cost.present && excluded != Some(DocumentFacetColumn::Cost))
            .bind(filters.cost.values.as_slice())
            .bind(filters.cost.include_null)
            .bind(
                filters.pipeline_time.present
                    && excluded != Some(DocumentFacetColumn::PipelineTime),
            )
            .bind(filters.pipeline_time.values.as_slice())
            .bind(filters.pipeline_time.include_null)
            .bind(filters.finished_at.present && excluded != Some(DocumentFacetColumn::FinishedAt))
            .bind(filters.finished_at.values.as_slice())
            .bind(filters.finished_at.include_null)
            .bind($snapshot_at)
            .bind($id_filter.is_some())
            .bind($id_filter.unwrap_or(&[]))
            .bind(ranges.file_size.map(|(min, _)| min))
            .bind(ranges.file_size.map(|(_, max)| max))
            .bind(ranges.pipeline_time.map(|(min, _)| min))
            .bind(ranges.pipeline_time.map(|(_, max)| max))
            .bind(ranges.date.map(|(min, _)| min))
            .bind(ranges.date.map(|(_, max)| max))
    }};
}

/// Typed facet-value range predicate for the selected column. Bounds are bound
/// as text and cast explicitly (`::text::bigint` / `::text::timestamptz`) so no
/// text parameter is ever inferred as a numeric wire type. `false` for columns
/// that accept no range hint.
fn document_facet_range_sql(column: DocumentFacetColumn) -> &'static str {
    match column {
        DocumentFacetColumn::FileSize => {
            "(j.revision_byte_size >= $45::text::bigint and j.revision_byte_size < $46::text::bigint)"
        }
        DocumentFacetColumn::PipelineTime => {
            "(j.pipeline_time_ms >= $45::text::bigint and j.pipeline_time_ms < $46::text::bigint)"
        }
        DocumentFacetColumn::UploadedAt => {
            "(j.created_at >= $45::text::timestamptz and j.created_at < $46::text::timestamptz)"
        }
        DocumentFacetColumn::FinishedAt => {
            "(j.finished_at >= $45::text::timestamptz and j.finished_at < $46::text::timestamptz)"
        }
        _ => "false",
    }
}

/// Per-column SQL for the facet aggregate: which canonical value slot is
/// populated, the value expression `facetSearch` matches, the deterministic
/// value ordering and the typed keyset predicate ($40 cursor, $41 null-bucket,
/// $39 facet search, $42 limit; $43 status pill, $44 facet statuses, $45/$46
/// facet range bounds).
struct DocumentFacetSql {
    value_text: &'static str,
    value_number: &'static str,
    value_timestamp: &'static str,
    value_decimal: &'static str,
    value_expr: &'static str,
    order_sql: &'static str,
    keyset_sql: &'static str,
}

fn document_facet_sql(column: DocumentFacetColumn) -> DocumentFacetSql {
    const TEXT_KEYSET: &str = "and (case when $41 then false when $40::text is null then true else (f.value_text is null or f.value_text > $40::text) end)";
    const NUMBER_KEYSET: &str = "and (case when $41 then false when $40::bigint is null then true else (f.value_number is null or f.value_number > $40::bigint) end)";
    const TIMESTAMP_KEYSET: &str = "and (case when $41 then false when $40::timestamptz is null then true else (f.value_timestamp is null or f.value_timestamp > $40::timestamptz) end)";
    const DECIMAL_KEYSET: &str = "and (case when $41 then false when $40::numeric is null then true else (f.value_decimal is null or f.value_decimal > $40::numeric) end)";
    match column {
        DocumentFacetColumn::FileName => DocumentFacetSql {
            value_text: "j.projection_file_name",
            value_number: "null::bigint",
            value_timestamp: "null::timestamptz",
            value_decimal: "null::numeric",
            value_expr: "j.projection_file_name",
            order_sql: "f.value_text asc nulls last",
            keyset_sql: TEXT_KEYSET,
        },
        DocumentFacetColumn::FileType => DocumentFacetSql {
            value_text: "j.revision_mime_type",
            value_number: "null::bigint",
            value_timestamp: "null::timestamptz",
            value_decimal: "null::numeric",
            value_expr: "j.revision_mime_type",
            order_sql: "f.value_text asc nulls last",
            keyset_sql: TEXT_KEYSET,
        },
        DocumentFacetColumn::Status => DocumentFacetSql {
            value_text: "j.derived_status",
            value_number: "null::bigint",
            value_timestamp: "null::timestamptz",
            value_decimal: "null::numeric",
            value_expr: "j.derived_status",
            order_sql: "f.value_text asc nulls last",
            keyset_sql: TEXT_KEYSET,
        },
        DocumentFacetColumn::FileSize => DocumentFacetSql {
            value_text: "null::text",
            value_number: "j.revision_byte_size",
            value_timestamp: "null::timestamptz",
            value_decimal: "null::numeric",
            value_expr: "j.revision_byte_size::text",
            order_sql: "f.value_number asc nulls last",
            keyset_sql: NUMBER_KEYSET,
        },
        DocumentFacetColumn::PipelineTime => DocumentFacetSql {
            value_text: "null::text",
            value_number: "j.pipeline_time_ms",
            value_timestamp: "null::timestamptz",
            value_decimal: "null::numeric",
            value_expr: "j.pipeline_time_ms::text",
            order_sql: "f.value_number asc nulls last",
            keyset_sql: NUMBER_KEYSET,
        },
        DocumentFacetColumn::UploadedAt => DocumentFacetSql {
            value_text: "null::text",
            value_number: "null::bigint",
            value_timestamp: "j.created_at",
            value_decimal: "null::numeric",
            value_expr: "to_char(j.created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
            order_sql: "f.value_timestamp asc nulls last",
            keyset_sql: TIMESTAMP_KEYSET,
        },
        DocumentFacetColumn::FinishedAt => DocumentFacetSql {
            value_text: "null::text",
            value_number: "null::bigint",
            value_timestamp: "j.finished_at",
            value_decimal: "null::numeric",
            value_expr: "coalesce(to_char(j.finished_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), '')",
            order_sql: "f.value_timestamp asc nulls last",
            keyset_sql: TIMESTAMP_KEYSET,
        },
        DocumentFacetColumn::Cost => DocumentFacetSql {
            value_text: "null::text",
            value_number: "null::bigint",
            value_timestamp: "null::timestamptz",
            value_decimal: "j.cost_total",
            value_expr: "j.cost_total::text",
            order_sql: "f.value_decimal asc nulls last",
            keyset_sql: DECIMAL_KEYSET,
        },
    }
}

/// Whether one document-list or document-facet read genuinely consumes
/// per-document cost, and so must be withheld while the library rollup
/// reconciles. One owner shared by the list and facet paths so neither can
/// approximate the other with a narrower predicate.
///
/// * `sort` — a cost-ordered list orders rows by the withheld value.
/// * `search` / `normalized_search` — a literal or Intl-normalized term matches
///   the canonical CTO cost expression over the same rows (see
///   `DOCUMENT_LIST_SEARCH_FIELD_EXPRESSIONS`), so the matching set itself
///   depends on the rollup generation.
/// * `filters` — a selected cost column filter values rows by the withheld
///   cost. The facet caller passes the raw filters; for the cost facet the
///   column check below is the full answer and the self-exclusion of that
///   column's filter is irrelevant.
/// * `facet_column` — `Some(Cost)` is cost-dependent by construction even
///   though the facet self-excludes its own filter: its values *are* the
///   withheld cost.
///
/// Duration/status/range/status-pill/indexing and id filters are not
/// cost-dependent, so a metadata read still serves real rows while pending.
pub fn document_scope_reads_cost(
    sort: Option<DocumentListSortColumn>,
    search: Option<&str>,
    normalized_search: Option<&str>,
    filters: &DocumentListFilters,
    facet_column: Option<DocumentFacetColumn>,
) -> bool {
    let text_search_present =
        |value: Option<&str>| value.is_some_and(|value| !value.trim().is_empty());
    facet_column == Some(DocumentFacetColumn::Cost)
        || sort == Some(DocumentListSortColumn::Cost)
        || filters.cost.present
        || text_search_present(search)
        || text_search_present(normalized_search)
}

/// Keyset-paginated fetch for the document list surface.
///
/// * `limit` is clamped to 1..=200 by the caller.
/// * `cursor` carries the complete ordering tuple for the selected sort:
///   primary value/null bucket plus `(created_at, id)` tie-breakers. Rows
///   strictly after that tuple in the requested direction are returned.
/// * `include_deleted` mirrors the query parameter on the HTTP surface.
/// * `search` applies a case-insensitive, literal substring filter to either
///   the persisted `external_key` or the rendered display file name.
/// * The join strategy is:
///   ```text
///   content_document
///     LEFT JOIN content_document_head ON (document_id)
///     LEFT JOIN content_revision       ON (active or readable)
///     LEFT JOIN content_mutation       ON (latest_mutation_id)
///     LEFT JOIN ingest_job             ON (mutation_id)
///     LEFT JOIN LATERAL ingest_attempt ON (job_id, attempt_number DESC)
///   ```
///   Every join is LEFT so documents without a head/mutation/job still show.
/// * A `None` `id_filter` leaves every document eligible; `Some(ids)` restricts
///   the page to exactly those documents, `Some(&[])` included — that one is an
///   empty page. An empty slice used to mean "no filter", which turned a caller
///   that had resolved a genuinely empty set into a request for the whole
///   library.
pub async fn list_document_page_rows(
    postgres: &PgPool,
    query: DocumentListPageQuery<'_>,
) -> Result<ContentDocumentListPage, sqlx::Error> {
    list_document_page_rows_with_executor(postgres, query).await
}

/// Row read shared by the public list and the snapshot-coherent
/// `read_document_list_page`. Takes an executor so the consolidated read can
/// run it inside its REPEATABLE READ transaction.
async fn list_document_page_rows_with_executor<'e, E>(
    executor: E,
    query: DocumentListPageQuery<'_>,
) -> Result<ContentDocumentListPage, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let DocumentListPageQuery {
        library_id,
        include_deleted,
        cursor,
        limit,
        search,
        normalized_search,
        search_statuses,
        filters,
        search_ranges,
        snapshot_at,
        sort,
        sort_desc,
        status_filter,
        id_filter,
    } = query;
    // Fetch `limit + 1` rows so we can report `has_more` without a COUNT(*).
    let fetch_limit = i64::from(limit) + 1;
    let search_pattern = normalize_search_pattern(search);
    let normalized_search_pattern = normalize_search_pattern(normalized_search);

    // Separate ORDER BY strings for the inner CTE (on `j.` alias inside
    // `joined`) and the outer SELECT (on `p.` alias inside `page`). They
    // share the same column set but live in different alias namespaces.
    // Every alternate sort falls back through `j.created_at desc,
    // j.id desc` as the secondary key so pagination stays deterministic
    // even when the primary column is NULL / tied.
    let (joined_order_sql, page_order_sql) = match (sort, sort_desc) {
        (DocumentListSortColumn::CreatedAt, true) => {
            ("j.created_at desc, j.id desc", "p.created_at desc, p.id desc")
        }
        (DocumentListSortColumn::CreatedAt, false) => {
            ("j.created_at asc, j.id asc", "p.created_at asc, p.id asc")
        }
        (DocumentListSortColumn::ExternalKey, true) => (
            "lower(j.projection_file_name) desc nulls last, j.created_at desc, j.id desc",
            "lower(p.projection_file_name) desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ExternalKey, false) => (
            "lower(j.projection_file_name) asc nulls last, j.created_at desc, j.id desc",
            "lower(p.projection_file_name) asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::MimeType, true) => (
            "j.revision_mime_type desc nulls last, j.created_at desc, j.id desc",
            "p.revision_mime_type desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::MimeType, false) => (
            "j.revision_mime_type asc nulls last, j.created_at desc, j.id desc",
            "p.revision_mime_type asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ByteSize, true) => (
            "j.revision_byte_size desc nulls last, j.created_at desc, j.id desc",
            "p.revision_byte_size desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ByteSize, false) => (
            "j.revision_byte_size asc nulls last, j.created_at desc, j.id desc",
            "p.revision_byte_size asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::DerivedStatus, true) => (
            "j.derived_status desc, j.created_at desc, j.id desc",
            "p.derived_status desc, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::DerivedStatus, false) => (
            "j.derived_status asc, j.created_at desc, j.id desc",
            "p.derived_status asc, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::Cost, true) => (
            "j.cost_total desc nulls last, j.created_at desc, j.id desc",
            "p.cost_total desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::Cost, false) => (
            "j.cost_total asc nulls last, j.created_at desc, j.id desc",
            "p.cost_total asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::PipelineTime, true) => (
            "j.pipeline_time_ms desc nulls last, j.created_at desc, j.id desc",
            "p.pipeline_time_ms desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::PipelineTime, false) => (
            "j.pipeline_time_ms asc nulls last, j.created_at desc, j.id desc",
            "p.pipeline_time_ms asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::FinishedAt, true) => (
            "j.finished_at desc nulls last, j.created_at desc, j.id desc",
            "p.finished_at desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::FinishedAt, false) => (
            "j.finished_at asc nulls last, j.created_at desc, j.id desc",
            "p.finished_at asc nulls last, p.created_at desc, p.id desc",
        ),
    };

    // Every supported sort uses a complete keyset: primary value (with an
    // explicit NULL bucket where applicable), then created_at and id as stable
    // tie-breakers. This avoids OFFSET drift when rows change between requests.
    let keyset_sql = match (sort, sort_desc) {
        (DocumentListSortColumn::CreatedAt, true) => {
            "and ($39::timestamptz is null or (j.created_at, j.id) < ($39, $40))"
        }
        (DocumentListSortColumn::CreatedAt, false) => {
            "and ($39::timestamptz is null or (j.created_at, j.id) > ($39, $40))"
        }
        (DocumentListSortColumn::ExternalKey, true) => {
            "and ($39::timestamptz is null or case when $44 then j.projection_file_name is null and (j.created_at, j.id) < ($39, $40) else j.projection_file_name is null or (lower(j.projection_file_name), j.created_at, j.id) < ($41, $39, $40) end)"
        }
        (DocumentListSortColumn::ExternalKey, false) => {
            "and ($39::timestamptz is null or case when $44 then j.projection_file_name is null and (j.created_at, j.id) < ($39, $40) else j.projection_file_name is null or lower(j.projection_file_name) > $41 or (lower(j.projection_file_name) = $41 and (j.created_at, j.id) < ($39, $40)) end)"
        }
        (DocumentListSortColumn::MimeType, true) => {
            "and ($39::timestamptz is null or case when $44 then j.revision_mime_type is null and (j.created_at, j.id) < ($39, $40) else j.revision_mime_type is null or (j.revision_mime_type, j.created_at, j.id) < ($41, $39, $40) end)"
        }
        (DocumentListSortColumn::MimeType, false) => {
            "and ($39::timestamptz is null or case when $44 then j.revision_mime_type is null and (j.created_at, j.id) < ($39, $40) else j.revision_mime_type is null or j.revision_mime_type > $41 or (j.revision_mime_type = $41 and (j.created_at, j.id) < ($39, $40)) end)"
        }
        (DocumentListSortColumn::ByteSize, true) => {
            "and ($39::timestamptz is null or case when $44 then j.revision_byte_size is null and (j.created_at, j.id) < ($39, $40) else j.revision_byte_size is null or (j.revision_byte_size, j.created_at, j.id) < ($42, $39, $40) end)"
        }
        (DocumentListSortColumn::ByteSize, false) => {
            "and ($39::timestamptz is null or case when $44 then j.revision_byte_size is null and (j.created_at, j.id) < ($39, $40) else j.revision_byte_size is null or j.revision_byte_size > $42 or (j.revision_byte_size = $42 and (j.created_at, j.id) < ($39, $40)) end)"
        }
        (DocumentListSortColumn::DerivedStatus, true) => {
            "and ($39::timestamptz is null or (j.derived_status, j.created_at, j.id) < ($41, $39, $40))"
        }
        (DocumentListSortColumn::DerivedStatus, false) => {
            "and ($39::timestamptz is null or j.derived_status > $41 or (j.derived_status = $41 and (j.created_at, j.id) < ($39, $40)))"
        }
        (DocumentListSortColumn::Cost, true) => {
            "and ($39::timestamptz is null or case when $44 then j.cost_total is null and (j.created_at, j.id) < ($39, $40) else j.cost_total is null or (j.cost_total, j.created_at, j.id) < ($41::text::numeric, $39, $40) end)"
        }
        (DocumentListSortColumn::Cost, false) => {
            "and ($39::timestamptz is null or case when $44 then j.cost_total is null and (j.created_at, j.id) < ($39, $40) else j.cost_total is null or j.cost_total > $41::text::numeric or (j.cost_total = $41::text::numeric and (j.created_at, j.id) < ($39, $40)) end)"
        }
        (DocumentListSortColumn::PipelineTime, true) => {
            "and ($39::timestamptz is null or case when $44 then j.pipeline_time_ms is null and (j.created_at, j.id) < ($39, $40) else j.pipeline_time_ms is null or (j.pipeline_time_ms, j.created_at, j.id) < ($42::bigint, $39, $40) end)"
        }
        (DocumentListSortColumn::PipelineTime, false) => {
            "and ($39::timestamptz is null or case when $44 then j.pipeline_time_ms is null and (j.created_at, j.id) < ($39, $40) else j.pipeline_time_ms is null or j.pipeline_time_ms > $42::bigint or (j.pipeline_time_ms = $42::bigint and (j.created_at, j.id) < ($39, $40)) end)"
        }
        (DocumentListSortColumn::FinishedAt, true) => {
            "and ($39::timestamptz is null or case when $44 then j.finished_at is null and (j.created_at, j.id) < ($39, $40) else j.finished_at is null or (j.finished_at, j.created_at, j.id) < ($43, $39, $40) end)"
        }
        (DocumentListSortColumn::FinishedAt, false) => {
            "and ($39::timestamptz is null or case when $44 then j.finished_at is null and (j.created_at, j.id) < ($39, $40) else j.finished_at is null or j.finished_at > $43 or (j.finished_at = $43 and (j.created_at, j.id) < ($39, $40)) end)"
        }
    };

    // `derived_status` mirrors apps/web/src/pages/documents/mappers.ts priority
    // chain on the Postgres-only signals we have in the list path. The 5
    // buckets the frontend filter surface exposes are:
    //   canceled / failed / processing / queued / ready
    // The graph_ready vs readable vs graph_sparse split is NOT part of this
    // derivation — that requires the knowledge revision state which isn't in
    // the CTE. `ready` here means "readable revision exists and no terminal
    // failure signal" — the inspector panel surfaces the finer split.
    // Same LATERAL protection as `aggregate_document_list_status_counts`:
    // a content_mutation can own many ingest_job rows (retry, requeue,
    // one bulk-import mutation can carry many document jobs), so
    // the join must return at most one job per document. The selected job
    // is from the newest mutation for this document; state priority is a
    // retry tie-breaker inside that mutation. The active revision is also
    // joined in the inner CTE so `ORDER BY revision_mime_type` /
    // `revision_byte_size` (the file-type / file-size column headers)
    // can push down into keyset sort.
    let sql = format!(
        "{base},
        page as (
            select * from base j
            where true
              {keyset_sql}
              and (cardinality($45::text[]) = 0 or j.derived_status = any($45))
            order by {joined_order_sql}
            limit $46
        )
        select
            p.id,
            p.workspace_id,
            p.library_id,
            p.external_key,
            p.projection_file_name,
            p.document_state,
            p.created_at,
            p.deleted_at,
            p.active_revision_id,
            p.readable_revision_id,
            p.revision_title,
            p.revision_mime_type,
            p.revision_byte_size,
            p.revision_source_uri,
            p.revision_document_hint,
            p.revision_content_source_kind,
            p.revision_storage_key,
            p.mutation_id,
            p.mutation_state,
            p.mutation_failure_code,
            p.mutation_requested_at,
            p.job_id,
            p.job_queue_state,
            p.job_queued_at,
            p.job_completed_at,
            p.attempt_current_stage,
            p.attempt_started_at,
            p.attempt_finished_at,
            p.attempt_failure_code,
            p.attempt_retryable,
            p.attempt_heartbeat_at,
            p.attempt_failure_message,
            p.attempt_progress_percent,
            p.finished_at,
            p.pipeline_time_ms,
            p.cost_total,
            p.cost_currency_code,
            p.billing_rollup_dirty,
            p.billing_rollup_terminal_error_code
        from page p
        order by {page_order_sql}",
        base = document_list_base_cte_sql(),
    );

    // Bind order: the fixed base layout $1..$38
    // (see `document_list_base_cte_sql`), then the page-specific
    //             $39 cursor_ts, $40 cursor_id, $41 cursor_text,
    //             $42 cursor_number, $43 cursor_finished, $44 cursor_is_null,
    //             $45 status_filter, $46 fetch_limit.
    //
    // `persistent(false)` forces each execution to re-plan using
    // concrete parameter values. Postgres caches prepared-statement
    // plans per connection and, after ~5 executions, switches to a
    // "generic plan" that ignores parameter values — on this query
    // (with highly selective `status_filter` / sort-column variants)
    // the generic plan collapses to a full sequential scan and ran
    // at ~4 s on the reference library even though the custom plan
    // finishes in 3 ms. Re-planning per call costs a few hundred µs
    // and keeps latency deterministic.
    let cursor_ts = cursor.map(|value| value.created_at);
    let cursor_id = cursor.map(|value| value.document_id);
    let cursor_text = cursor.and_then(|value| value.text_value.as_deref());
    let cursor_number = cursor.and_then(|value| value.number_value);
    let cursor_finished = cursor.and_then(|value| value.finished_value);
    let cursor_is_null = cursor.is_some_and(|value| value.value_is_null);
    let query =
        sqlx::query_as::<_, ContentDocumentListRow>(sqlx::AssertSqlSafe(&*sql)).persistent(false);
    let query = bind_document_list_base!(
        query,
        library_id,
        include_deleted,
        search_pattern,
        normalized_search_pattern,
        search_statuses,
        filters,
        search_ranges,
        snapshot_at,
        id_filter,
        None
    );
    let query = query
        .bind(cursor_ts)
        .bind(cursor_id)
        .bind(cursor_text)
        .bind(cursor_number)
        .bind(cursor_finished)
        .bind(cursor_is_null)
        .bind(status_filter)
        .bind(fetch_limit);

    let mut rows = query.fetch_all(executor).await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.truncate(limit as usize);
    }
    Ok(ContentDocumentListPage { rows, has_more })
}

/// Per-bucket counts matching the `derived_status` column the list CTE
/// emits. Used by the documents page filter strip to populate pill
/// badges without an extra endpoint round-trip.
#[derive(Debug, Clone, Default, FromRow)]
pub struct DocumentListStatusCountsRow {
    pub total: Option<i64>,
    pub ready: Option<i64>,
    pub processing: Option<i64>,
    pub queued: Option<i64>,
    pub failed: Option<i64>,
    pub canceled: Option<i64>,
    pub empty: Option<i64>,
}

/// One pass over the same CASE derivation used by `list_document_page_rows`
/// producing the 6 bucket counts plus the overall total. Called only when
/// the caller opts in via `includeTotal=true` — otherwise every page-flip
/// would pay for an unbounded aggregate.
pub async fn aggregate_document_list_status_counts(
    postgres: &PgPool,
    query: DocumentListCountQuery<'_>,
) -> Result<DocumentListStatusCountsRow, sqlx::Error> {
    aggregate_document_list_status_counts_with_executor(postgres, query).await
}

async fn aggregate_document_list_status_counts_with_executor<'e, E>(
    executor: E,
    query: DocumentListCountQuery<'_>,
) -> Result<DocumentListStatusCountsRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let DocumentListCountQuery {
        library_id,
        include_deleted,
        search,
        normalized_search,
        search_statuses,
        filters,
        search_ranges,
        status_filter,
        id_filter,
        snapshot_at,
    } = query;
    let search_pattern = normalize_search_pattern(search);
    let normalized_search_pattern = normalize_search_pattern(normalized_search);
    // The base CTE is shared with `list_document_page_rows` and the facet
    // aggregate so the pill counts, the page and the facet counts can never
    // disagree about which rows carry a given `derived_status`. The status pill
    // and the resolved id filter are applied here exactly as the page applies
    // them, so `includeTotal` describes the page's own set.
    let sql = format!(
        "{base}
        select
            count(*)::bigint as total,
            count(*) filter (where derived_status = 'ready')::bigint as ready,
            count(*) filter (where derived_status = 'processing')::bigint as processing,
            count(*) filter (where derived_status = 'queued')::bigint as queued,
            count(*) filter (where derived_status = 'failed')::bigint as failed,
            count(*) filter (where derived_status = 'canceled')::bigint as canceled,
            count(*) filter (where derived_status = 'empty')::bigint as empty
        from base
        where (cardinality($39::text[]) = 0 or derived_status = any($39))",
        base = document_list_base_cte_sql(),
    );
    let query = sqlx::query_as::<_, DocumentListStatusCountsRow>(sqlx::AssertSqlSafe(&*sql));
    let query = bind_document_list_base!(
        query,
        library_id,
        include_deleted,
        search_pattern,
        normalized_search_pattern,
        search_statuses,
        filters,
        search_ranges,
        snapshot_at,
        id_filter,
        None
    );
    let query = query.bind(status_filter);
    query.fetch_one(executor).await
}

/// One statement's worth of scope facts for a document-list or facet read:
/// the library rollup health, the whole-scope NULL-cost probe, the scope row
/// count, and the six derived-status bucket counts. Read in the same
/// REPEATABLE READ snapshot as the rows it describes, so health is observable
/// even when the page has zero rows and a concurrent dirty generation cannot
/// be observed by only part of one response.
#[derive(Debug, Clone, FromRow)]
pub struct DocumentListScopeRead {
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub row_count: i64,
    pub has_null_cost: bool,
    pub total: Option<i64>,
    pub ready: Option<i64>,
    pub processing: Option<i64>,
    pub queued: Option<i64>,
    pub failed: Option<i64>,
    pub canceled: Option<i64>,
    pub empty: Option<i64>,
}

impl DocumentListScopeRead {
    #[must_use]
    pub fn status_counts(&self) -> DocumentListStatusCountsRow {
        DocumentListStatusCountsRow {
            total: self.total,
            ready: self.ready,
            processing: self.processing,
            queued: self.queued,
            failed: self.failed,
            canceled: self.canceled,
            empty: self.empty,
        }
    }
}

/// Runs the scope aggregate through an executor. `exclude` disables exactly
/// the selected facet column's own filter (as the facet read does); the list
/// path passes `None` so the cost filter is honored in its row count and the
/// returned page can never be advertised with a total that ignored it.
async fn document_list_scope_read_with_executor<'e, E>(
    executor: E,
    query: DocumentListCountQuery<'_>,
    exclude: Option<DocumentFacetColumn>,
) -> Result<DocumentListScopeRead, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let DocumentListCountQuery {
        library_id,
        include_deleted,
        search,
        normalized_search,
        search_statuses,
        filters,
        search_ranges,
        status_filter,
        id_filter,
        snapshot_at,
    } = query;
    let search_pattern = normalize_search_pattern(search);
    let normalized_search_pattern = normalize_search_pattern(normalized_search);
    // `document_list_base_cte_sql` already embeds `billing_health`. The
    // aggregate selects that health, the scope-wide row count, the derived
    // status buckets (the status pill narrows them exactly as it narrows the
    // page) and the whole-scope NULL-cost probe in one statement. `exclude`
    // keeps the selected facet column's filter out of its own NULL-cost scan.
    let sql = format!(
        "{base},
        scope_health as (
            select rollup_dirty, terminal_error_code from billing_health
        ),
        scope_facts as (
            select
                count(*)::bigint as row_count,
                count(*) filter (where cardinality($39::text[]) = 0 or derived_status = any($39))::bigint as total,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'ready')::bigint as ready,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'processing')::bigint as processing,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'queued')::bigint as queued,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'failed')::bigint as failed,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'canceled')::bigint as canceled,
                count(*) filter (where (cardinality($39::text[]) = 0 or derived_status = any($39)) and derived_status = 'empty')::bigint as empty,
                exists (select 1 from base j where j.cost_total is null) as has_null_cost
            from base
        )
        select
            scope_health.rollup_dirty,
            scope_health.terminal_error_code,
            scope_facts.row_count,
            scope_facts.has_null_cost,
            scope_facts.total,
            scope_facts.ready,
            scope_facts.processing,
            scope_facts.queued,
            scope_facts.failed,
            scope_facts.canceled,
            scope_facts.empty
        from scope_health cross join scope_facts",
        base = document_list_base_cte_sql(),
    );
    let query = sqlx::query_as::<_, DocumentListScopeRead>(sqlx::AssertSqlSafe(&*sql));
    let query = bind_document_list_base!(
        query,
        library_id,
        include_deleted,
        search_pattern,
        normalized_search_pattern,
        search_statuses,
        filters,
        search_ranges,
        snapshot_at,
        id_filter,
        exclude
    );
    let query = query.bind(status_filter);
    query.fetch_one(executor).await
}

/// One page of documents with its scope facts read in a single REPEATABLE READ
/// snapshot. The page cannot be served from a different generation than the
/// health marker, and health is observable even when the page has zero rows.
pub struct DocumentListPageRead {
    pub page: ContentDocumentListPage,
    pub scope: DocumentListScopeRead,
}

pub async fn read_document_list_page(
    postgres: &PgPool,
    page_query: DocumentListPageQuery<'_>,
    count_query: DocumentListCountQuery<'_>,
) -> Result<DocumentListPageRead, sqlx::Error> {
    read_document_list_page_after_scope_read(postgres, page_query, count_query, || async {}).await
}

/// Snapshot-coherent list read with a barrier between the scope aggregate and
/// the page. Only the snapshot-isolation integration test drives the barrier:
/// it commits a dirty rollup generation on a second connection there and
/// proves the page still describes the snapshot the scope facts came from.
pub async fn read_document_list_page_after_scope_read<F, Fut>(
    postgres: &PgPool,
    page_query: DocumentListPageQuery<'_>,
    count_query: DocumentListCountQuery<'_>,
    after_scope_read: F,
) -> Result<DocumentListPageRead, sqlx::Error>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut transaction = postgres.begin().await?;
    sqlx::query("set transaction isolation level repeatable read, read only")
        .execute(&mut *transaction)
        .await?;
    // The list path applies every effective filter, including the cost column
    // filter, so its total/count domain matches the rows and the status pill.
    let scope =
        document_list_scope_read_with_executor(&mut *transaction, count_query, None).await?;
    after_scope_read().await;
    let page = list_document_page_rows_with_executor(&mut *transaction, page_query).await?;
    transaction.commit().await?;
    Ok(DocumentListPageRead { page, scope })
}

/// One page of facet values with the library rollup health from the same
/// REPEATABLE READ snapshot. `has_null_cost` is the whole-scope mixed-currency
/// probe the cost facet still fails closed on.
pub struct DocumentFacetRead {
    pub page: DocumentFacetPage,
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub has_null_cost: bool,
}

pub async fn read_document_facet_page(
    postgres: &PgPool,
    facet_query: DocumentListFacetQuery<'_>,
    count_query: DocumentListCountQuery<'_>,
) -> Result<DocumentFacetRead, sqlx::Error> {
    let column = facet_query.column;
    let mut transaction = postgres.begin().await?;
    sqlx::query("set transaction isolation level repeatable read, read only")
        .execute(&mut *transaction)
        .await?;
    // The facet self-excludes its own column before the scope aggregate, so a
    // selected cost filter narrows the NULL-cost probe of every *other* facet.
    let scope =
        document_list_scope_read_with_executor(&mut *transaction, count_query, Some(column))
            .await?;
    let page = list_document_facet_rows_with_executor(&mut *transaction, facet_query).await?;
    transaction.commit().await?;
    Ok(DocumentFacetRead {
        page,
        rollup_dirty: scope.rollup_dirty,
        terminal_error_code: scope.terminal_error_code,
        has_null_cost: scope.has_null_cost,
    })
}

/// One page of distinct values for a single grid column, keyset-paginated on
/// the canonical value. Self-excludes the selected column's own filter and
/// applies every other filter/search/scope predicate over the same base CTE as
/// the list, so a facet count and the page it opens cannot disagree.
pub async fn list_document_facet_rows(
    postgres: &PgPool,
    query: DocumentListFacetQuery<'_>,
) -> Result<DocumentFacetPage, sqlx::Error> {
    list_document_facet_rows_with_executor(postgres, query).await
}

/// Facet read shared by the public facet surface and the snapshot-coherent
/// `read_document_facet_page`. Takes an executor so the consolidated read can
/// run it inside its REPEATABLE READ transaction.
async fn list_document_facet_rows_with_executor<'e, E>(
    executor: E,
    query: DocumentListFacetQuery<'_>,
) -> Result<DocumentFacetPage, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let DocumentListFacetQuery {
        library_id,
        include_deleted,
        column,
        search,
        normalized_search,
        search_statuses,
        facet_search,
        facet_search_statuses,
        facet_search_range,
        filters,
        search_ranges,
        status_filter,
        snapshot_at,
        cursor,
        limit,
        id_filter,
    } = query;
    let fetch_limit = i64::from(limit) + 1;
    let search_pattern = normalize_search_pattern(search);
    let normalized_search_pattern = normalize_search_pattern(normalized_search);
    let facet_search_pattern = normalize_search_pattern(facet_search);
    let (facet_range_min, facet_range_max) = match facet_search_range.as_ref() {
        Some((min, max)) => (Some(min.as_str()), Some(max.as_str())),
        None => (None, None),
    };

    let facet = document_facet_sql(column);
    let sql = format!(
        "{base},
        facet as (
            select
                {value_text} as value_text,
                {value_number} as value_number,
                {value_timestamp} as value_timestamp,
                {value_decimal} as value_decimal,
                count(*)::bigint as facet_count
            from base j
            where (cardinality($43::text[]) = 0 or j.derived_status = any($43))
              and (
                    ($39::text is null and cardinality($44::text[]) = 0 and $45::text is null)
                    or ($39::text is not null and lower(({value_expr})::text) like $39 escape '\\')
                    or (cardinality($44::text[]) > 0 and ({value_expr}) = any($44))
                    or ($45::text is not null and {range_sql})
                  )
            group by 1, 2, 3, 4
        )
        select f.value_text, f.value_number, f.value_timestamp, f.value_decimal, f.facet_count
        from facet f
        where true
          {keyset_sql}
        order by {order_sql}
        limit $42",
        base = document_list_base_cte_sql(),
        value_text = facet.value_text,
        value_number = facet.value_number,
        value_timestamp = facet.value_timestamp,
        value_decimal = facet.value_decimal,
        value_expr = facet.value_expr,
        keyset_sql = facet.keyset_sql,
        order_sql = facet.order_sql,
        range_sql = document_facet_range_sql(column),
    );

    let (cursor_text, cursor_number, cursor_timestamp, cursor_decimal) = match cursor {
        Some(DocumentFacetCursorValue::Text(value)) => (Some(value.as_str()), None, None, None),
        Some(DocumentFacetCursorValue::Number(value)) => (None, Some(*value), None, None),
        Some(DocumentFacetCursorValue::Timestamp(value)) => (None, None, Some(*value), None),
        Some(DocumentFacetCursorValue::Decimal(value)) => (None, None, None, Some(*value)),
        Some(DocumentFacetCursorValue::Null) | None => (None, None, None, None),
    };
    let cursor_is_null = matches!(cursor, Some(DocumentFacetCursorValue::Null));

    let query = sqlx::query_as::<_, DocumentFacetRow>(sqlx::AssertSqlSafe(&*sql));
    let query = bind_document_list_base!(
        query,
        library_id,
        include_deleted,
        search_pattern,
        normalized_search_pattern,
        search_statuses,
        filters,
        search_ranges,
        snapshot_at,
        id_filter,
        Some(column)
    );
    let query = query.bind(facet_search_pattern);
    let query = match column {
        DocumentFacetColumn::FileName
        | DocumentFacetColumn::FileType
        | DocumentFacetColumn::Status => query.bind(cursor_text),
        DocumentFacetColumn::FileSize | DocumentFacetColumn::PipelineTime => {
            query.bind(cursor_number)
        }
        DocumentFacetColumn::UploadedAt | DocumentFacetColumn::FinishedAt => {
            query.bind(cursor_timestamp)
        }
        DocumentFacetColumn::Cost => query.bind(cursor_decimal),
    };
    let query = query
        .bind(cursor_is_null)
        .bind(fetch_limit)
        .bind(status_filter)
        .bind(facet_search_statuses)
        .bind(facet_range_min)
        .bind(facet_range_max);

    let mut rows = query.fetch_all(executor).await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.truncate(limit as usize);
    }
    Ok(DocumentFacetPage { rows, has_more })
}

/// Counts published active documents that are both `ready` on the document
/// list surface and represented by a document node in one graph projection
/// with evidence from the exact currently readable revision.
///
/// This is an identity intersection rather than a comparison of two unrelated
/// aggregate counts. A stale graph node for a deleted or no-longer-ready
/// document therefore cannot compensate for a ready document that is missing
/// from the projection.
pub async fn count_library_ready_documents_in_graph_projection(
    postgres: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    count_library_ready_documents_in_graph_projection_with_executor(
        postgres,
        library_id,
        projection_version,
    )
    .await
}

/// Returns the dashboard-ready denominator and exact-revision projected
/// numerator using the caller's existing database snapshot.
///
/// This helper deliberately does not begin or commit a transaction. Callers
/// that require a stable cross-query view must supply a connection whose
/// transaction isolation has already been established.
pub async fn count_library_ready_and_projected_documents_with_executor(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
) -> Result<(i64, i64), sqlx::Error> {
    count_library_ready_and_projected_documents_after_ready_read_with_executor(
        connection,
        library_id,
        projection_version,
        std::future::ready(()),
    )
    .await
}

/// Returns the ready-document denominator and exact-revision projected
/// numerator from one repeatable-read snapshot.
///
/// Graph rebuild publication must never combine counts observed before and
/// after a concurrent document publication. Besides producing impossible
/// percentages, that race could incorrectly pass the graph quality guard.
pub async fn count_library_ready_and_projected_documents_in_snapshot(
    postgres: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<(i64, i64), sqlx::Error> {
    count_library_ready_and_projected_documents_in_snapshot_inner(
        postgres,
        library_id,
        projection_version,
        std::future::ready(()),
    )
    .await
}

/// Test seam that pauses after the denominator establishes its MVCC snapshot.
#[cfg(feature = "test-support")]
pub async fn count_library_ready_and_projected_documents_after_ready_read_for_test<F>(
    postgres: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_ready_read: F,
) -> Result<(i64, i64), sqlx::Error>
where
    F: Future<Output = ()>,
{
    count_library_ready_and_projected_documents_in_snapshot_inner(
        postgres,
        library_id,
        projection_version,
        after_ready_read,
    )
    .await
}

async fn count_library_ready_and_projected_documents_in_snapshot_inner<F>(
    postgres: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_ready_read: F,
) -> Result<(i64, i64), sqlx::Error>
where
    F: Future<Output = ()>,
{
    let mut transaction = postgres.begin().await?;
    sqlx::query("set transaction isolation level repeatable read, read only")
        .execute(&mut *transaction)
        .await?;
    let counts = count_library_ready_and_projected_documents_after_ready_read_with_executor(
        &mut transaction,
        library_id,
        projection_version,
        after_ready_read,
    )
    .await?;
    transaction.commit().await?;
    Ok(counts)
}

async fn count_library_ready_and_projected_documents_after_ready_read_with_executor<F>(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    after_ready_read: F,
) -> Result<(i64, i64), sqlx::Error>
where
    F: Future<Output = ()>,
{
    let empty_filters = DocumentListFilters::default();
    let empty_ranges = DocumentListSearchRanges::default();
    let status = aggregate_document_list_status_counts_with_executor(
        &mut *connection,
        DocumentListCountQuery {
            library_id,
            include_deleted: false,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &empty_filters,
            search_ranges: &empty_ranges,
            status_filter: &[],
            id_filter: None,
            snapshot_at: Utc::now(),
        },
    )
    .await?;
    after_ready_read.await;
    let ready = status.ready.unwrap_or_default();
    let projected = count_library_ready_documents_in_graph_projection_with_executor(
        &mut *connection,
        library_id,
        projection_version,
    )
    .await?
    .clamp(0, ready);
    Ok((ready, projected))
}

/// The ready-document population shared by the graph-ready counter and the
/// documents-list `indexing=graph_ready|graph_gap` filters. One CTE so the
/// counter the dashboard shows and the set an operator is sent to cannot
/// disagree: active, canonically published, and derived `ready` under the same policy
/// `aggregate_document_list_status_counts` uses.
fn library_ready_documents_cte_sql() -> String {
    format!(
        "select d.id, current_revision.revision_id as readable_revision_id
         from content_document d
         {CURRENT_DOCUMENT_REVISION_JOIN_SQL}
         left join content_document_head h on h.document_id = d.id
         left join content_mutation m on m.id = h.latest_mutation_id
         left join lateral (
             {LATEST_DOCUMENT_INGEST_JOB_SQL}
         ) ij on true
         where d.library_id = $1
           and d.document_state = 'active'
           and d.deleted_at is null
           and ({DERIVED_STATUS_CASE_SQL}) = 'ready'"
    )
}

/// Whether the active graph projection carries a document node for `ready`'s
/// exact readable revision. The evidence join fences a node left behind by an
/// older readable revision.
const READY_DOCUMENT_GRAPH_PROJECTION_EXISTS_SQL: &str = "exists (
            select 1
            from runtime_graph_node node
            join runtime_graph_evidence evidence
              on evidence.library_id = node.library_id
             and evidence.target_kind = 'node'
             and evidence.target_id = node.id
             and evidence.document_id = ready.id
             and evidence.revision_id = ready.readable_revision_id
            where node.library_id = $1
              and node.projection_version = $2
              and node.node_type = 'document'
              and node.document_id = ready.id
        )";

/// The active projection version for the graph-coverage predicates, or `0`
/// when no non-empty projection exists. `0` matches no node, so an absent
/// projection yields an empty `graph_ready` set and a `graph_gap` of every
/// ready document — the same numbers the metric derives from
/// `snapshot.projection_version.max(1)`.
async fn active_graph_projection_version(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let snapshot =
        crate::infra::repositories::get_runtime_graph_snapshot_with_executor(postgres, library_id)
            .await?;
    Ok(match snapshot {
        Some(snapshot) if snapshot.graph_status != "empty" && snapshot.node_count > 0 => {
            snapshot.projection_version.max(1)
        }
        _ => 0,
    })
}

async fn count_library_ready_documents_in_graph_projection_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    // Keep the readiness derivation and per-document latest-job selection
    // identical to `aggregate_document_list_status_counts`. In particular, a
    // leased or failed retry must not be reported as graph-ready merely because
    // the previous readable revision still has a projection node.
    let sql = format!(
        "with ready_documents as ({cte})
        select count(*)::bigint
        from ready_documents ready
        where {projection_exists}",
        cte = library_ready_documents_cte_sql(),
        projection_exists = READY_DOCUMENT_GRAPH_PROJECTION_EXISTS_SQL,
    );
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .bind(projection_version)
        .fetch_one(executor)
        .await
}

/// Ready documents the active graph projection covers, by document id. The exact
/// set behind the dashboard's `graphReady` counter.
pub async fn list_library_documents_graph_ready(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let projection_version = active_graph_projection_version(postgres, library_id).await?;
    Ok(load_library_graph_coverage_rows(postgres, library_id, projection_version)
        .await?
        .into_iter()
        .filter_map(|(document_id, graph_ready)| graph_ready.then_some(document_id))
        .collect())
}

/// Ready documents the active graph projection does not cover, by document id.
/// The exact set behind the dashboard's `graphSparse` counter.
pub async fn list_library_documents_with_graph_gap(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let projection_version = active_graph_projection_version(postgres, library_id).await?;
    Ok(load_library_graph_coverage_rows(postgres, library_id, projection_version)
        .await?
        .into_iter()
        .filter_map(|(document_id, graph_ready)| (!graph_ready).then_some(document_id))
        .collect())
}

async fn load_library_graph_coverage_rows<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<(Uuid, bool)>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let sql = format!(
        "with ready_documents as ({cte})
        select ready.id, ({projection_exists}) as graph_ready
        from ready_documents ready
        order by ready.id",
        cte = library_ready_documents_cte_sql(),
        projection_exists = READY_DOCUMENT_GRAPH_PROJECTION_EXISTS_SQL,
    );
    sqlx::query_as::<_, (Uuid, bool)>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(executor)
        .await
}

/// Transaction-bound coverage counts used by guarded graph publication.
///
/// The supplied document/revision is counted as ready even though the same
/// outer transaction has not yet moved its job to `completed`; every other
/// identity uses the exact document-list derived-status policy, including its
/// latest-job selection. Projected coverage is the identity intersection with
/// document nodes in the active projection carrying evidence from each exact
/// readable revision.
pub async fn count_library_graph_coverage_for_publication_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    publishing_document_id: Uuid,
    publishing_revision_id: Uuid,
) -> Result<(i64, i64), sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let sql = format!(
        "with ready_documents as (
            select d.id, case
                when d.id = $3 and h.active_revision_id = $4 and h.readable_revision_id = $4
                then $4 else current_revision.revision_id
            end as readable_revision_id
            from content_document d
            {CURRENT_DOCUMENT_REVISION_JOIN_SQL}
            left join content_document_head h on h.document_id = d.id
            left join content_mutation m on m.id = h.latest_mutation_id
            left join lateral (
                {LATEST_DOCUMENT_INGEST_JOB_SQL}
            ) ij on true
            where d.library_id = $1
              and d.document_state = 'active'
              and d.deleted_at is null
              and (
                   ({DERIVED_STATUS_CASE_SQL}) = 'ready'
                   or (
                       d.id = $3
                       and h.active_revision_id = $4
                       and h.readable_revision_id = $4
                   )
              )
        )
        select
            count(*)::bigint,
            count(*) filter (where exists (
                select 1
                from runtime_graph_node as node
                join runtime_graph_evidence as evidence
                  on evidence.library_id = node.library_id
                 and evidence.target_kind = 'node'
                 and evidence.target_id = node.id
                 and evidence.document_id = ready.id
                 and evidence.revision_id = ready.readable_revision_id
                where node.library_id = $1
                  and node.projection_version = $2
                  and node.node_type = 'document'
                  and node.document_id = ready.id
            ))::bigint
        from ready_documents as ready"
    );
    sqlx::query_as::<_, (i64, i64)>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(projection_version)
        .bind(publishing_document_id)
        .bind(publishing_revision_id)
        .fetch_one(executor)
        .await
}

/// Counts active documents whose current effective revision has at least one
/// typed fact. The effective revision matches the document-summary read path:
/// prefer the readable revision, then fall back to the active revision while a
/// document is still being prepared.
pub async fn count_library_documents_with_typed_facts(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    count_library_documents_with_typed_facts_with_executor(postgres, library_id).await
}

async fn count_library_documents_with_typed_facts_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
) -> Result<i64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from knowledge_document d
         join knowledge_structured_revision sr
           on sr.revision_id = coalesce(d.readable_revision_id, d.active_revision_id)
          and sr.document_id = d.document_id
          and sr.workspace_id = d.workspace_id
          and sr.library_id = d.library_id
          and sr.preparation_state = 'prepared'
         where d.library_id = $1
           and d.document_state <> 'deleted'
           and d.deleted_at is null
           and sr.typed_fact_count > 0",
    )
    .bind(library_id)
    .fetch_one(executor)
    .await
}

/// Cross-surface library facts observed from one MVCC snapshot.
#[derive(Debug, Clone)]
pub struct LibraryDashboardSnapshot {
    pub metrics: ironrag_contracts::documents::LibraryDocumentMetrics,
    pub graph_snapshot: Option<crate::infra::repositories::RuntimeGraphSnapshotRow>,
    pub typed_fact_document_count: i64,
    /// Oldest claimable `ingest_job` in this library, read from the same
    /// repeatable-read snapshot as the metrics above; `None` when nothing is
    /// waiting. Feeds the ops coverage notice's stalled-queue check so it cannot
    /// be combined with counts observed at a different instant.
    pub oldest_available_queued_job_at: Option<DateTime<Utc>>,
}

/// Loads every database-backed dashboard/summary aggregate in one
/// repeatable-read, read-only transaction.
pub async fn load_library_dashboard_snapshot(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<LibraryDashboardSnapshot, sqlx::Error> {
    load_library_dashboard_snapshot_inner(postgres, library_id, std::future::ready(())).await
}

/// Authoritative per-library document metrics row. This is the ONE
/// function every surface (`/ops/libraries/{id}/dashboard`,
/// `/content/libraries/{id}/documents?includeTotal=true`,
/// `/knowledge/libraries/{id}/summary`) should route through for
/// document-count numbers. It runs the status-bucket aggregate and
/// the graph-ready identity intersection and preserves the invariant
/// `graph_ready + graph_sparse == ready` always holds on the wire,
/// even during a graph rebuild where the two halves are briefly
/// out-of-sync.
///
/// Contract:
///   * `total == ready + processing + queued + failed + canceled + empty`
///   * `graph_ready + graph_sparse == ready`
///
/// Scoped to `document_state = 'active'` (deleted documents are not
/// reflected in any of the metrics). Search filtering and
/// include-deleted live only on the list surface — metrics are a
/// library-wide summary.
pub async fn aggregate_library_document_metrics(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<ironrag_contracts::documents::LibraryDocumentMetrics, sqlx::Error> {
    Ok(load_library_dashboard_snapshot(postgres, library_id).await?.metrics)
}

/// Test-support seam that pauses the dashboard read after its status aggregate
/// has established the repeatable-read snapshot. Integration tests use it to
/// commit a concurrent status/projection publication before the remaining
/// reads and prove that the response cannot mix database snapshots.
#[cfg(feature = "test-support")]
pub async fn aggregate_library_document_metrics_after_status_read_for_test<F>(
    postgres: &PgPool,
    library_id: Uuid,
    after_status_read: F,
) -> Result<ironrag_contracts::documents::LibraryDocumentMetrics, sqlx::Error>
where
    F: Future<Output = ()>,
{
    Ok(load_library_dashboard_snapshot_after_status_read_for_test(
        postgres,
        library_id,
        after_status_read,
    )
    .await?
    .metrics)
}

/// Test-support seam exposing the complete cross-surface snapshot after the
/// first aggregate establishes its repeatable-read view.
#[cfg(feature = "test-support")]
pub async fn load_library_dashboard_snapshot_after_status_read_for_test<F>(
    postgres: &PgPool,
    library_id: Uuid,
    after_status_read: F,
) -> Result<LibraryDashboardSnapshot, sqlx::Error>
where
    F: Future<Output = ()>,
{
    load_library_dashboard_snapshot_inner(postgres, library_id, after_status_read).await
}

async fn load_library_dashboard_snapshot_inner<F>(
    postgres: &PgPool,
    library_id: Uuid,
    after_status_read: F,
) -> Result<LibraryDashboardSnapshot, sqlx::Error>
where
    F: Future<Output = ()>,
{
    // A dashboard response spans the status aggregate, active graph snapshot,
    // exact ready/document-node identity intersection, and typed-fact count.
    // Keep all reads on one connection and one MVCC snapshot: separate READ
    // COMMITTED statements can otherwise combine facts from different graph
    // or document revisions.
    // Passing the transaction executor through every helper also prevents a
    // max_connections=1 pool from self-deadlocking while the transaction owns
    // its only connection.
    let mut transaction = postgres.begin().await?;
    sqlx::query("set transaction isolation level repeatable read, read only")
        .execute(&mut *transaction)
        .await?;
    let empty_filters = DocumentListFilters::default();
    let empty_ranges = DocumentListSearchRanges::default();
    let status_row = aggregate_document_list_status_counts_with_executor(
        &mut *transaction,
        DocumentListCountQuery {
            library_id,
            include_deleted: false,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &empty_filters,
            search_ranges: &empty_ranges,
            status_filter: &[],
            id_filter: None,
            snapshot_at: Utc::now(),
        },
    )
    .await?;
    after_status_read.await;
    let snapshot_row = crate::infra::repositories::get_runtime_graph_snapshot_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    let graph_ready_raw = if let Some(snapshot) = snapshot_row.as_ref() {
        if snapshot.graph_status == "empty" || snapshot.node_count <= 0 {
            0
        } else {
            count_library_ready_documents_in_graph_projection_with_executor(
                &mut *transaction,
                library_id,
                snapshot.projection_version.max(1),
            )
            .await?
        }
    } else {
        0
    };
    let typed_fact_document_count =
        count_library_documents_with_typed_facts_with_executor(&mut *transaction, library_id)
            .await?;
    let oldest_available_queued_job_at =
        crate::infra::repositories::ops_repository::oldest_available_queued_job_at_with_executor(
            &mut *transaction,
            library_id,
        )
        .await?;
    let total = status_row.total.unwrap_or(0);
    let ready = status_row.ready.unwrap_or(0);
    let processing = status_row.processing.unwrap_or(0);
    let queued = status_row.queued.unwrap_or(0);
    let failed = status_row.failed.unwrap_or(0);
    let canceled = status_row.canceled.unwrap_or(0);
    let empty = status_row.empty.unwrap_or(0);
    // The SQL already counts a subset of the exact ready identities. Keep the
    // defensive clamp at the wire boundary so corrupted legacy rows cannot
    // violate the published invariant.
    let graph_ready = graph_ready_raw.clamp(0, ready);
    let graph_sparse = ready.saturating_sub(graph_ready);
    let snapshot = LibraryDashboardSnapshot {
        metrics: ironrag_contracts::documents::LibraryDocumentMetrics {
            total,
            ready,
            processing,
            queued,
            failed,
            canceled,
            empty,
            graph_ready,
            graph_sparse,
            recomputed_at: chrono::Utc::now(),
        },
        graph_snapshot: snapshot_row,
        typed_fact_document_count,
        oldest_available_queued_job_at,
    };
    transaction.commit().await?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    /// IRG-236: the one cost-dependence predicate both the list and the facet
    /// paths use must flag every read that genuinely consumes cost — a cost
    /// sort, a cost filter, a raw or Intl-normalized search (the search fields
    /// include the CTO cost expression) and the cost facet — while leaving a
    /// metadata read available.
    #[test]
    fn document_scope_reads_cost_covers_sort_filter_search_and_facet_column() {
        use super::{
            DocumentFacetColumn, DocumentListFilters, DocumentListSortColumn, FilterValues,
            document_scope_reads_cost,
        };
        let plain = DocumentListFilters::default();
        let cost_filtered = DocumentListFilters {
            cost: FilterValues {
                present: true,
                include_null: false,
                values: vec![rust_decimal::Decimal::new(5, 0)],
            },
            ..Default::default()
        };

        assert!(
            document_scope_reads_cost(Some(DocumentListSortColumn::Cost), None, None, &plain, None,),
            "a cost sort consumes cost"
        );
        assert!(
            document_scope_reads_cost(None, Some("acme"), None, &plain, None),
            "a raw text search matches the cost expression"
        );
        assert!(
            document_scope_reads_cost(None, None, Some("acme"), &plain, None),
            "a normalized-only search matches the cost expression"
        );
        assert!(
            document_scope_reads_cost(None, None, None, &cost_filtered, None),
            "a cost column filter consumes cost"
        );
        assert!(
            document_scope_reads_cost(None, None, None, &plain, Some(DocumentFacetColumn::Cost),),
            "the cost facet is cost-dependent even though it self-excludes its filter"
        );
        assert!(
            document_scope_reads_cost(
                None,
                Some("acme"),
                None,
                &plain,
                Some(DocumentFacetColumn::FileType),
            ),
            "a non-cost facet under a cost-matching search is cost-dependent"
        );
        assert!(
            !document_scope_reads_cost(
                Some(DocumentListSortColumn::CreatedAt),
                None,
                None,
                &plain,
                None,
            ),
            "a metadata list read is not cost-dependent"
        );
        assert!(
            !document_scope_reads_cost(
                None,
                None,
                None,
                &plain,
                Some(DocumentFacetColumn::FileType),
            ),
            "a non-cost facet without a cost predicate stays available"
        );
    }
}
