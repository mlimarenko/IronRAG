mod deletion;
mod libraries;
mod library_queries;
mod workspaces;

pub use self::{deletion::*, libraries::*, library_queries::*, workspaces::*};

#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
pub use ironrag_contracts::admin::AdminLibraryFacetColumn;
use ironrag_contracts::admin::{AdminLibraryCostSearch, AdminLibraryCostValue};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

const DEFAULT_ADMIN_LIBRARY_CURRENCY_CODE: &str = "USD";

#[derive(Debug, Clone, FromRow)]
pub struct CatalogWorkspaceRow {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CatalogLibraryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub web_ingest_policy: Value,
    pub recognition_policy: Value,
    pub retrieval_config: Value,
    pub lifecycle_state: String,
    pub include_document_hint_in_mcp_answers: bool,
    pub chunking_template: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct CatalogLibraryPageRows {
    pub items: Vec<CatalogLibraryRow>,
    pub total_count: i64,
}

pub struct CatalogLibraryPageQuery<'a> {
    pub workspace_id: Uuid,
    pub search: Option<&'a str>,
    pub visible_library_ids: Option<&'a [Uuid]>,
    pub pinned_library_id: Option<Uuid>,
    pub offset: u32,
    pub limit: u32,
}

/// One column's selected filter values. `present` false imposes no constraint;
/// `include_null` selects the column's own NULL bucket; several values are OR
/// within the column and AND across columns.
#[derive(Debug, Clone)]
pub struct AdminLibraryFilterValues<T> {
    pub present: bool,
    pub include_null: bool,
    pub values: Vec<T>,
}

impl<T> Default for AdminLibraryFilterValues<T> {
    fn default() -> Self {
        Self { present: false, include_null: false, values: Vec::new() }
    }
}

/// Strictly typed per-column grid filters over the canonical displayed values.
#[derive(Debug, Clone, Default)]
pub struct AdminLibraryListFilters {
    pub library: AdminLibraryFilterValues<String>,
    pub workspace: AdminLibraryFilterValues<String>,
    pub documents: AdminLibraryFilterValues<i64>,
    pub queue: AdminLibraryFilterValues<i64>,
    pub cost: AdminLibraryFilterValues<AdminLibraryCostValue>,
    pub calls: AdminLibraryFilterValues<i64>,
    pub readiness: AdminLibraryFilterValues<bool>,
    pub lifecycle: AdminLibraryFilterValues<String>,
}

/// The administration grid column type, owned by the contract and shared by the
/// sort key, the filter key and the facet column so the three cannot drift.
impl AdminLibraryListFilters {
    /// Drop exactly one column's constraint so a facet can self-exclude its own
    /// filter while every other predicate and the parameter count stay stable.
    #[must_use]
    pub fn without(mut self, column: AdminLibraryFacetColumn) -> Self {
        match column {
            AdminLibraryFacetColumn::Library => self.library = AdminLibraryFilterValues::default(),
            AdminLibraryFacetColumn::Workspace => {
                self.workspace = AdminLibraryFilterValues::default();
            }
            AdminLibraryFacetColumn::Documents => {
                self.documents = AdminLibraryFilterValues::default();
            }
            AdminLibraryFacetColumn::Queue => self.queue = AdminLibraryFilterValues::default(),
            AdminLibraryFacetColumn::Cost => self.cost = AdminLibraryFilterValues::default(),
            AdminLibraryFacetColumn::Calls => self.calls = AdminLibraryFilterValues::default(),
            AdminLibraryFacetColumn::Readiness => {
                self.readiness = AdminLibraryFilterValues::default();
            }
            AdminLibraryFacetColumn::Lifecycle => {
                self.lifecycle = AdminLibraryFilterValues::default();
            }
        }
        self
    }
}

/// The cost facet's extra grouping columns: the ISO currency and the coverage.
/// Every other column projects NULLs so the facet SELECT keeps one shape.
#[must_use]
pub const fn admin_library_facet_extra_sql(column: AdminLibraryFacetColumn) -> &'static str {
    match column {
        AdminLibraryFacetColumn::Cost => {
            "case when total_cost_value is not null then currency_code end as value_currency, \
              case when total_cost_value is not null then coverage_state end as value_coverage"
        }
        _ => "null::text as value_currency, null::text as value_coverage",
    }
}

/// The typed value projection of a facet column over the `filtered` CTE.
#[must_use]
pub const fn admin_library_facet_value_sql(column: AdminLibraryFacetColumn) -> &'static str {
    match column {
        AdminLibraryFacetColumn::Library => {
            "display_name as value_text, null::bigint as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Workspace => {
            "workspace_display_name as value_text, null::bigint as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Lifecycle => {
            "lifecycle_state as value_text, null::bigint as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Readiness => {
            "null::text as value_text, null::bigint as value_number, \
             null::numeric as value_decimal, ingestion_ready as value_flag"
        }
        AdminLibraryFacetColumn::Documents => {
            "null::text as value_text, document_count_value as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Queue => {
            "null::text as value_text, queued_job_count as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Calls => {
            "null::text as value_text, provider_call_count_value as value_number, \
             null::numeric as value_decimal, null::bool as value_flag"
        }
        AdminLibraryFacetColumn::Cost => {
            "null::text as value_text, \
              case when total_cost_value is not null then unpriced_provider_call_count end as value_number, \
             round(total_cost_value, 2) as value_decimal, null::bool as value_flag"
        }
    }
}

/// The typed keyset predicate for this column's own cursor value.
#[must_use]
pub const fn admin_library_facet_cursor_sql(column: AdminLibraryFacetColumn) -> &'static str {
    match column {
        AdminLibraryFacetColumn::Library
        | AdminLibraryFacetColumn::Workspace
        | AdminLibraryFacetColumn::Lifecycle => {
            "(not $42::bool or value_text > $35::text or value_text is null)"
        }
        AdminLibraryFacetColumn::Documents
        | AdminLibraryFacetColumn::Queue
        | AdminLibraryFacetColumn::Calls => {
            "(not $42::bool or value_number > $36::bigint or value_number is null)"
        }
        AdminLibraryFacetColumn::Cost => {
            "(not $42::bool or value_decimal is null \
              or (value_currency, value_decimal, value_coverage, value_number) \
                 > ($39::text, $37::numeric, $40::text, $41::bigint))"
        }
        AdminLibraryFacetColumn::Readiness => {
            "(not $42::bool or value_flag > $38::bool or value_flag is null)"
        }
    }
}

/// The typed column a facet keyset orders on.
#[must_use]
pub const fn admin_library_facet_order_sql(column: AdminLibraryFacetColumn) -> &'static str {
    match column {
        AdminLibraryFacetColumn::Library
        | AdminLibraryFacetColumn::Workspace
        | AdminLibraryFacetColumn::Lifecycle => "value_text",
        AdminLibraryFacetColumn::Documents
        | AdminLibraryFacetColumn::Queue
        | AdminLibraryFacetColumn::Calls => "value_number",
        AdminLibraryFacetColumn::Cost => {
            "(value_decimal is null) asc, value_currency asc nulls last, \
              value_decimal asc nulls last, value_coverage asc nulls last, value_number"
        }
        AdminLibraryFacetColumn::Readiness => "value_flag",
    }
}

/// One typed keyset value for a facet page. The variant is fixed by the facet
/// column, so a cursor from another column is rejected by the HTTP layer.
#[derive(Debug, Clone)]
pub enum AdminLibraryFacetCursorValue {
    Text(String),
    Number(i64),
    /// The cost column's keyset: the ISO currency primary, then the rounded display amount.
    Cost {
        currency: String,
        amount: Decimal,
        coverage: String,
        unpriced_call_count: i64,
    },
    Flag(bool),
}

/// One distinct facet value with its row count; exactly the slots the column
/// owns are set (none for the NULL group). Cost adds its currency and coverage.
#[derive(Debug, Clone, Default, FromRow)]
pub struct AdminLibraryFacetRow {
    pub value_text: Option<String>,
    pub value_number: Option<i64>,
    pub value_decimal: Option<Decimal>,
    pub value_flag: Option<bool>,
    pub value_currency: Option<String>,
    pub value_coverage: Option<String>,
    pub facet_count: i64,
}

/// One bounded page of distinct facet values.
pub struct AdminLibraryFacetPage {
    pub rows: Vec<AdminLibraryFacetRow>,
    pub has_more: bool,
}

/// Sort column plus direction for the administration library page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminLibraryPageOrder {
    pub column: AdminLibraryFacetColumn,
    pub desc: bool,
}

impl AdminLibraryPageOrder {
    /// NULLS LAST on both directions, with `display_name`/`id` as the stable
    /// tiebreak so the same page never reorders between requests.
    pub fn sql(self) -> String {
        let direction = if self.desc { "desc" } else { "asc" };
        let primary = match self.column {
            AdminLibraryFacetColumn::Library => format!("lower(display_name) {direction}"),
            AdminLibraryFacetColumn::Workspace => {
                format!("lower(workspace_display_name) {direction} nulls last")
            }
            AdminLibraryFacetColumn::Documents => {
                format!("document_count_value {direction} nulls last")
            }
            AdminLibraryFacetColumn::Queue => format!("queued_job_count {direction} nulls last"),
            AdminLibraryFacetColumn::Cost => format!(
                "(total_cost_value is null) asc, \
                  case when total_cost_value is not null then currency_code end {direction}, \
                  round(total_cost_value, 2) {direction} nulls last, \
                  case when total_cost_value is not null then coverage_state end {direction}, \
                  case when total_cost_value is not null then unpriced_provider_call_count end {direction}"
            ),
            AdminLibraryFacetColumn::Calls => {
                format!("provider_call_count_value {direction} nulls last")
            }
            AdminLibraryFacetColumn::Readiness => {
                format!("ingestion_ready {direction} nulls last")
            }
            AdminLibraryFacetColumn::Lifecycle => {
                format!("lifecycle_state {direction} nulls last")
            }
        };
        format!("{primary}, lower(display_name) {direction}, id {direction}")
    }
}

/// The typed whole-table search: the raw and Intl-normalized terms over the
/// text columns, plus the typed hints over the non-text columns. Every present
/// alternative is OR'd; no term is a lexicon lookup.
#[derive(Debug, Clone, Copy)]
pub struct AdminLibrarySearchScope<'a> {
    pub search: Option<&'a str>,
    pub normalized_search: Option<&'a str>,
    pub numbers: &'a [i64],
    pub costs: &'a [AdminLibraryCostSearch],
    /// The term names the unavailable / unknown label the blocked columns show.
    pub unavailable: bool,
    pub readiness: &'a [bool],
    pub lifecycle: &'a [String],
}

impl<'a> AdminLibrarySearchScope<'a> {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            search: None,
            normalized_search: None,
            numbers: &[],
            costs: &[],
            unavailable: false,
            readiness: &[],
            lifecycle: &[],
        }
    }
}

pub struct AdminLibraryPageQuery<'a> {
    pub scope: AdminLibrarySearchScope<'a>,
    pub filters: &'a AdminLibraryListFilters,
    pub required_binding_purposes: &'a [String],
    pub offset: u32,
    pub limit: u32,
    pub order: AdminLibraryPageOrder,
}

/// One page of distinct values for a facet column, self-excluding that column's
/// own filter and bounded by the column's own value search and keyset cursor.
pub struct AdminLibraryFacetQuery<'a> {
    pub scope: AdminLibrarySearchScope<'a>,
    pub filters: &'a AdminLibraryListFilters,
    pub required_binding_purposes: &'a [String],
    pub column: AdminLibraryFacetColumn,
    pub facet_search: Option<&'a str>,
    pub facet_hints: AdminLibrarySearchScope<'a>,
    pub cursor: Option<&'a AdminLibraryFacetCursorValue>,
    pub limit: u32,
}

#[derive(Debug, Clone, FromRow)]
pub struct AdminLibraryPageRow {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
    pub include_document_hint_in_mcp_answers: bool,
    pub workspace_id: Uuid,
    pub workspace_slug: String,
    pub workspace_display_name: String,
    pub missing_binding_purposes: Vec<String>,
    pub queued_job_count: i64,
    pub cost_blocked: bool,
    pub currency_count: i64,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
    pub coverage_state: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AdminLibraryFacetsRow {
    pub total_count: i64,
    pub workspace_count: i64,
    pub ready_count: i64,
    pub active_count: i64,
    pub cost_blocked: bool,
    pub currency_count: i64,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
}

#[derive(Debug, Clone, Copy, FromRow)]
pub struct CatalogLibraryIdentityRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogDeleteBlockers {
    pub undispatched_webhook_lifecycle_events: usize,
    pub active_webhook_delivery_jobs: usize,
    pub unresolved_webhook_delivery_attempts: usize,
}

impl CatalogDeleteBlockers {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.undispatched_webhook_lifecycle_events == 0
            && self.active_webhook_delivery_jobs == 0
            && self.unresolved_webhook_delivery_attempts == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogLibraryDeleteOutcome {
    Deleted,
    NotFound,
    Blocked(CatalogDeleteBlockers),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogWorkspaceDeleteOutcome {
    Deleted,
    NotFound,
    Blocked(CatalogDeleteBlockers),
}
