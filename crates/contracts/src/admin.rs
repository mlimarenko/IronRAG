//! Administration list contracts consumed by the web console.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::AiBindingPurpose;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Billing coverage rendered for an administration library row.
pub enum AdminLibraryCostCoverage {
    /// Every provider call is priced.
    Complete,
    /// Some provider calls are not priced.
    Partial,
    /// No provider call has a configured price.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Workspace identity rendered beside an administration library row.
pub struct AdminLibraryWorkspaceSummary {
    /// Workspace identifier.
    pub id: Uuid,
    /// Stable workspace slug.
    pub slug: String,
    /// Human-readable workspace name.
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Ingestion binding state rendered for an administration library row.
pub struct AdminLibraryIngestionReadiness {
    /// Whether every ingestion binding required by the library is available.
    pub ready: bool,
    /// Required ingestion bindings that are not available.
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Billed usage rendered for one administration library row.
pub struct AdminLibraryCostSummary {
    /// Decimal cost encoded as a string to preserve precision.
    pub total_cost: String,
    /// ISO currency code shared by the priced calls.
    pub currency_code: String,
    /// Documents represented by the billing rollup.
    pub document_count: i64,
    /// Provider calls represented by the billing rollup.
    pub provider_call_count: i64,
    /// Provider calls for which no price was configured.
    pub unpriced_provider_call_count: i64,
    /// Whether the reported total covers all provider calls.
    pub cost_coverage: AdminLibraryCostCoverage,
}

/// Displayed cost identity shared by cells, facets, filters, search and sorting.
///
/// `amount` is the rounded display decimal (two fraction digits by default),
/// kept as source text
/// so no float ever touches it, and `currency_code` scopes it so an amount never
/// silently matches another currency's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminLibraryCostValue {
    /// Rounded display amount, exact decimal source string.
    pub amount: String,
    /// ISO 4217 code of the single currency the amount is denominated in.
    pub currency_code: String,
    /// Whether the amount covers all provider calls.
    pub coverage: AdminLibraryCostCoverage,
    /// Provider calls for which no price was configured.
    pub unpriced_call_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Narrow library row rendered by the administration list.
pub struct AdminLibraryListItem {
    /// Library identifier.
    pub id: Uuid,
    /// Stable library slug.
    pub slug: String,
    /// Human-readable library name.
    pub display_name: String,
    /// Current library lifecycle state.
    pub lifecycle_state: String,
    /// Whether MCP answers include the document hint configured by operators.
    pub include_document_hint_in_mcp_answers: bool,
    /// Owning workspace identity.
    pub workspace: AdminLibraryWorkspaceSummary,
    /// Ingestion readiness shown by the list and filters.
    pub ingestion_readiness: AdminLibraryIngestionReadiness,
    /// Ingest jobs waiting in this library's queue (`queue_state = 'queued'`).
    pub queued_job_count: i64,
    /// Cost summary when the billing rollup is readable and uses one currency.
    pub cost_summary: Option<AdminLibraryCostSummary>,
    /// Canonical cost projection for the grid; null when the rollup is blocked,
    /// mixes currencies, or is completely unpriced (the cell's unknown label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_value: Option<AdminLibraryCostValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Counts used by the administration library summary and filters.
pub struct AdminLibraryFacets {
    /// Distinct workspaces represented by the filtered result.
    pub workspace_count: i64,
    /// Filtered libraries ready for ingestion.
    pub ready_count: i64,
    /// Filtered libraries in the active lifecycle state.
    pub active_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Bounded administration library page.
pub struct AdminLibraryPageResponse {
    /// Rows in the requested page.
    pub items: Vec<AdminLibraryListItem>,
    /// Number of rows matching the server-side filters over the whole dataset.
    pub total_count: i64,
    /// Counts for the current server-side filter set.
    pub facets: AdminLibraryFacets,
    /// Aggregate billed usage for the filtered result when it is safe to report.
    pub aggregate_cost_summary: Option<AdminLibraryCostSummary>,
}

/// Columns the administration library grid can sort, filter and facet on. Ids
/// are the server's own vocabulary: the grid column id, the sort key and the
/// facet column all agree on one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminLibraryFacetColumn {
    /// Library display name.
    Library,
    /// Owning workspace display name.
    Workspace,
    /// Documents represented by the billing rollup.
    Documents,
    /// Queued ingest jobs.
    Queue,
    /// Billed cost total.
    Cost,
    /// Billed provider calls.
    Calls,
    /// Ingestion readiness.
    Readiness,
    /// Library lifecycle state.
    Lifecycle,
}

/// Multi-value filters for each library grid column.
///
/// A nullable array per column: an absent key, whole-key `null` or `[]` means
/// "no constraint"; a `null` element
/// selects the column's own NULL bucket; several values are OR within the column
/// and AND across columns. Values are the canonical displayed values, so a
/// decimal cost keeps its source text and a blocked cost's documents/calls stay
/// a real NULL rather than a fabricated zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminLibraryColumnFilters {
    /// Library display names.
    pub library: Option<Vec<Option<String>>>,
    /// Workspace display names.
    pub workspace: Option<Vec<Option<String>>>,
    /// Billed document counts; `null` is the blocked-rollup bucket.
    pub documents: Option<Vec<Option<i64>>>,
    /// Queued ingest job counts.
    pub queue: Option<Vec<Option<i64>>>,
    /// Billed cost values as `{amount, currencyCode, coverage, unpricedCallCount}`.
    pub cost: Option<Vec<Option<AdminLibraryCostValue>>>,
    /// Billed provider call counts; `null` is the blocked-rollup bucket.
    pub calls: Option<Vec<Option<i64>>>,
    /// Ingestion readiness booleans.
    pub readiness: Option<Vec<Option<bool>>>,
    /// Lifecycle state values.
    pub lifecycle: Option<Vec<Option<String>>>,
}

/// Constraints derived from one displayed cost search term. Fields are AND
/// within one term; several terms are OR. No amount changes currency.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminLibraryCostSearch {
    /// Exact rounded display amount, when the term names one.
    pub amount: Option<String>,
    /// Displayed ISO currency code, when present in the term.
    pub currency_code: Option<String>,
    /// Billing coverage indicated by the displayed label.
    pub coverage: Option<AdminLibraryCostCoverage>,
    /// Unpriced call count indicated by the displayed label.
    pub unpriced_call_count: Option<i64>,
}

/// Typed alternatives to literal whole-table search.
///
/// Hints derived from the term the operator typed: the canonical numbers a
/// formatted documents/queue/calls
/// label spells, the canonical rounded decimals a cost label carries, whether
/// the term names the unavailable/unknown label, and the readiness / lifecycle
/// values whose localized label matches. Each present group is an OR
/// alternative to the literal text search, never a lexicon.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminLibrarySearchHints {
    /// Exact values the documents, queue and calls columns display.
    pub numbers: Option<Vec<i64>>,
    /// Displayed cost constraints, preserving currency and partial-cost metadata.
    pub costs: Option<Vec<AdminLibraryCostSearch>>,
    /// The term names the unavailable / unknown label the blocked columns show.
    pub unavailable: Option<bool>,
    /// Readiness booleans whose localized label matches the term.
    pub readiness: Option<Vec<bool>>,
    /// Lifecycle values whose localized label matches the term.
    pub lifecycle: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// One distinct facet value with its row count.
///
/// Exactly the typed slots the column owns are present: `text` for
/// library/workspace/lifecycle, `number`
/// for documents/queue/calls, and `decimal` + `currencyCode` + `coverage` +
/// `unpricedCallCount` for cost. `flag` is readiness. The
/// column's NULL bucket carries none of them.
pub struct AdminLibraryFacetValue {
    /// Libraries carrying this value within the self-excluded filter set.
    pub count: i64,
    /// A text value (library, workspace, lifecycle).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// An integer value (documents, queue, calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    /// The cost column's canonical rounded display amount, as source text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimal: Option<String>,
    /// The readiness boolean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<bool>,
    /// The cost amount's single ISO currency code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency_code: Option<String>,
    /// The cost amount's billing coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<AdminLibraryCostCoverage>,
    /// Unpriced provider calls in the displayed cost value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpriced_call_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// One bounded, self-excluding page of distinct values for a facet column.
pub struct AdminLibraryFacetsResponse {
    /// The column whose values this page describes.
    pub column: AdminLibraryFacetColumn,
    /// Distinct values ordered ascending with the NULL bucket last.
    pub values: Vec<AdminLibraryFacetValue>,
    /// `null` when the last page has been reached.
    pub next_cursor: Option<String>,
}
