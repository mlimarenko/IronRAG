mod provider_calls;
pub(crate) mod provider_reservation;
mod reads;
mod usage;

use usage::*;

use std::{collections::HashSet, time::Duration};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        agent_runtime::RuntimeTaskKind,
        billing::{
            BillingCharge, BillingExecutionCost, BillingExecutionOwnerKind, BillingProviderCall,
            CostCoverageStatus, CostRollupFreshness, PricingBillingUnit,
        },
        provider_profiles::ProviderUsagePolicy,
    },
    infra::repositories::billing_repository,
    interfaces::http::router_support::ApiError,
};

#[cfg(feature = "test-support")]
use crate::infra::repositories::runtime_repository;

const EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS: [u64; 2] = [10, 50];
const PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS: [u64; 2] = [10, 50];
const MIXED_CURRENCY_TERMINAL_ERROR_CODE: &str = "mixed_currency";

/// Call kind of one ingest chunk-embedding provider request. The late capture
/// command and the reservation constructor share this single literal so the
/// ledger cannot record two different names for the same paid stage.
pub(crate) const EMBED_CHUNK_CALL_KIND: &str = "embed_chunk";

/// Pricing completeness of one derived rollup row, from the observations the
/// rollup stores.
///
/// `unpriced_provider_call_count` covers completed calls with no usage or no
/// price *and* reservations that never completed, so it cannot be compared with
/// the settled (completed) call count: one priced call plus one unresolved
/// reservation is `partial`, not `unknown`. The rollup does not store the priced
/// count, so `partial` is claimed only when a nonzero charge proves that some
/// call was priced; a zero total with unresolved observations stays `unknown`
/// instead of reading as free.
#[must_use]
pub(crate) fn derived_cost_coverage(
    unpriced_provider_call_count: i64,
    total_cost: Decimal,
) -> CostCoverageStatus {
    if unpriced_provider_call_count <= 0 {
        CostCoverageStatus::Complete
    } else if total_cost > Decimal::ZERO {
        CostCoverageStatus::Partial
    } else {
        CostCoverageStatus::Unknown
    }
}

/// Cost fields for one rolled-up scope, derived from the derived row and its
/// call counts.
///
/// A stored derived row is not itself evidence that a price was observed:
/// [`CostCoverageStatus::from_call_counts`] reports `complete` for a scope with
/// no calls, and historical rows were written that way. So the projection
/// requires positive measurement evidence of its own:
///
/// - `has_derived_cost == false`, or a row reporting no settled and no unpriced
///   call: nothing was observed, so no total, currency or coverage is reported;
/// - a nonzero charged total: a real charge was observed, so the row is at least
///   `partial` even while other calls in scope stay unresolved;
/// - no unpriced observation at all: every settled call is priced, which includes
///   a genuine zero from a price catalog that prices at zero.
///
/// A scope with unresolved or unpriced observations and no observed charge stays
/// `None` with `unknown` coverage instead of reading as free.
#[must_use]
pub(crate) fn measured_scope_cost(
    has_derived_cost: bool,
    total_cost: Decimal,
    currency_code: &str,
    provider_call_count: i64,
    unpriced_provider_call_count: i64,
) -> (Option<Decimal>, Option<String>, Option<CostCoverageStatus>) {
    if !has_derived_cost {
        return (None, None, None);
    }
    if provider_call_count == 0 && unpriced_provider_call_count == 0 {
        // The derived row exists but records no dispatch and no liability:
        // that is an absent measurement, not a measured free scope.
        return (None, None, None);
    }
    let coverage = derived_cost_coverage(unpriced_provider_call_count, total_cost);
    if coverage == CostCoverageStatus::Unknown {
        return (None, None, Some(coverage));
    }
    (Some(total_cost), Some(currency_code.to_string()), Some(coverage))
}

/// Whether a mapped execution cost records any provider-call observation.
///
/// Neither a settled nor an unpriced call means the derived rollup observed
/// nothing. That is not evidence that a provider price was observed, so it
/// cannot be served as a cost — and it is not a measured zero either.
#[must_use]
fn execution_cost_has_observation(
    provider_call_count: i32,
    unpriced_provider_call_count: i32,
) -> bool {
    provider_call_count > 0 || unpriced_provider_call_count > 0
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCostSummary {
    pub document_id: Uuid,
    /// Cost measured by this document's ledger charges. `None` when the document
    /// has no derived cost row or when no call in it carries a price;
    /// `cost_coverage` distinguishes the two.
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    /// Calls whose call state is `completed`: a settled observation, not proof
    /// that a price was observed.
    pub provider_call_count: i64,
    /// Calls whose liability no charge proves, including reservations that never
    /// completed.
    pub unpriced_provider_call_count: i64,
    /// Pricing completeness of the document's calls. `None` when no execution
    /// produced a derived cost row for it at all.
    pub cost_coverage: Option<CostCoverageStatus>,
}

/// Library document-cost list with the freshness of its owning rollup. When
/// `cost_rollup_freshness` is `Reconciling` the `items` are withheld: a
/// per-document cost cannot be served exactly while a library execution is
/// mid-reconciliation, and an empty list would otherwise be indistinguishable
/// from a library that truly has no costs.
#[derive(Debug, Clone)]
pub struct DocumentCostList {
    pub cost_rollup_freshness: CostRollupFreshness,
    pub items: Vec<DocumentCostSummary>,
}

/// One offset page over [`DocumentCostList`], carrying the freshness so the
/// HTTP layer can mark a reconciling page without inventing rows.
#[derive(Debug, Clone)]
pub struct DocumentCostListPage {
    pub cost_rollup_freshness: CostRollupFreshness,
    pub items: Vec<DocumentCostSummary>,
    pub has_more: bool,
    pub total: Option<i64>,
}

/// One keyset/offset page of a billing sub-resource, generic over the item
/// shape (provider calls, charges, document costs). `total` is only
/// populated when the caller opted into the expensive aggregate count.
#[derive(Debug, Clone)]
pub struct BillingSubResourcePage<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub total: Option<i64>,
}

/// One keyset page request for a billing sub-resource attributed to a single
/// execution. Both provider-call and charge listings share this input so the
/// page contract (owner kind, execution, cursor, limit, total opt-in) cannot
/// drift between them.
#[derive(Debug, Clone, Copy)]
pub struct BillingExecutionPageQuery {
    pub execution_kind: BillingExecutionOwnerKind,
    pub execution_id: Uuid,
    pub cursor: Option<(DateTime<Utc>, Uuid)>,
    pub limit: i64,
    pub include_total: bool,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCostSummary {
    /// Freshness of the owning library rollup, independent of `costCoverage`
    /// (pricing). While `reconciling` every value field below is `null`: a
    /// stale aggregate is never served as exact and a fake zero is never
    /// substituted.
    pub cost_rollup_freshness: CostRollupFreshness,
    /// Charged cost of the library. `None` when no execution in it has a
    /// derived cost row, or when every dispatchable call is unpriced:
    /// `cost_coverage` distinguishes unmeasured from unknown.
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub document_count: Option<i64>,
    pub provider_call_count: Option<i64>,
    pub unpriced_provider_call_count: Option<i64>,
    pub cost_coverage: Option<CostCoverageStatus>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCostSummary {
    /// Freshness of the owning workspace rollup: any one dirty library makes
    /// the workspace aggregate stale. See [`LibraryCostSummary`].
    pub cost_rollup_freshness: CostRollupFreshness,
    /// Charged cost of the workspace. `None` under the same rule as
    /// [`LibraryCostSummary::total_cost`].
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub library_count: Option<i64>,
    pub document_count: Option<i64>,
    pub provider_call_count: Option<i64>,
    pub unpriced_provider_call_count: Option<i64>,
    pub cost_coverage: Option<CostCoverageStatus>,
}

#[derive(Debug, Clone)]
pub struct CaptureQueryExecutionBillingCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub execution_id: Uuid,
    pub runtime_execution_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub call_kind: String,
    pub usage_json: Value,
}

#[derive(Debug, Clone)]
pub struct CaptureExecutionBillingCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<RuntimeTaskKind>,
    pub binding_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub call_kind: String,
    pub usage_json: Value,
}

/// Attribution required to durably reserve a provider call before any paid
/// network request starts. The provider/model catalog ids come from the
/// already-resolved binding, avoiding a second name-based lookup.
#[derive(Debug, Clone)]
pub struct ReserveExecutionProviderCallCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<RuntimeTaskKind>,
    pub binding_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: String,
}

#[derive(Debug, Clone)]
pub struct CaptureIngestAttemptBillingCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub attempt_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub call_kind: String,
    pub usage_json: Value,
}

#[derive(Debug, Clone)]
pub struct CaptureGraphExtractionBillingCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub graph_extraction_id: Uuid,
    pub runtime_execution_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: Value,
}

#[derive(Clone, Default)]
pub struct BillingService;

/// Workspace, library and optional document of one billing owning execution.
struct BillingExecutionScope {
    workspace_id: Uuid,
    library_id: Uuid,
    knowledge_document_id: Option<Uuid>,
}

impl BillingService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Captures provider-call billing for a query execution.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when attribution or persistence fails.
    #[cfg(feature = "test-support")]
    pub async fn capture_query_execution(
        &self,
        state: &AppState,
        command: CaptureQueryExecutionBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        self.capture_execution_provider_call(
            state,
            CaptureExecutionBillingCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                owning_execution_kind: "query_execution".to_string(),
                owning_execution_id: command.execution_id,
                runtime_execution_id: Some(command.runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                binding_id: command.binding_id,
                provider_kind: command.provider_kind,
                model_name: command.model_name,
                call_kind: command.call_kind,
                usage_json: command.usage_json,
            },
        )
        .await
    }

    /// Captures provider-call billing for an ingest attempt.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when attribution or persistence fails.
    #[cfg(feature = "test-support")]
    pub async fn capture_ingest_attempt(
        &self,
        state: &AppState,
        command: CaptureIngestAttemptBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        self.capture_execution_provider_call(
            state,
            CaptureExecutionBillingCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                owning_execution_kind: "ingest_attempt".to_string(),
                owning_execution_id: command.attempt_id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                binding_id: command.binding_id,
                provider_kind: command.provider_kind,
                model_name: command.model_name,
                call_kind: command.call_kind,
                usage_json: command.usage_json,
            },
        )
        .await
    }

    /// Captures provider-call billing for a graph extraction attempt.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when attribution or persistence fails.
    #[cfg(feature = "test-support")]
    pub async fn capture_graph_extraction(
        &self,
        state: &AppState,
        command: CaptureGraphExtractionBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        self.capture_execution_provider_call(
            state,
            CaptureExecutionBillingCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                owning_execution_kind: "graph_extraction_attempt".to_string(),
                owning_execution_id: command.graph_extraction_id,
                runtime_execution_id: Some(command.runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::GraphExtract),
                binding_id: command.binding_id,
                provider_kind: command.provider_kind,
                model_name: command.model_name,
                call_kind: "graph_extract".to_string(),
                usage_json: command.usage_json,
            },
        )
        .await
    }

    #[cfg(feature = "test-support")]
    async fn validate_runtime_attribution(
        &self,
        state: &AppState,
        owning_execution_kind: BillingExecutionOwnerKind,
        owning_execution_id: Uuid,
        runtime_execution_id: Option<Uuid>,
        runtime_task_kind: Option<RuntimeTaskKind>,
    ) -> Result<(), ApiError> {
        match (runtime_execution_id, runtime_task_kind) {
            (None, None) => Ok(()),
            (Some(_), None) | (None, Some(_)) => Err(ApiError::Conflict(
                "runtime billing attribution requires both runtime_execution_id and runtime_task_kind"
                    .to_string(),
            )),
            (Some(runtime_execution_id), Some(runtime_task_kind)) => {
                let runtime_execution =
                    runtime_repository::get_runtime_execution_by_id(
                        &state.persistence.postgres,
                        runtime_execution_id,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                        ApiError::resource_not_found("runtime_execution", runtime_execution_id)
                    })?;
                if runtime_execution.owner_kind.as_str()
                    != execution_owner_kind_key(owning_execution_kind)
                {
                    return Err(ApiError::Conflict(format!(
                        "runtime execution {} belongs to owner kind {}, not {}",
                        runtime_execution_id,
                        runtime_execution.owner_kind.as_str(),
                        execution_owner_kind_key(owning_execution_kind)
                    )));
                }
                if runtime_execution.owner_id != owning_execution_id {
                    return Err(ApiError::Conflict(format!(
                        "runtime execution {} belongs to owner {}, not {}",
                        runtime_execution_id, runtime_execution.owner_id, owning_execution_id
                    )));
                }
                if runtime_execution.task_kind != runtime_task_kind {
                    return Err(ApiError::Conflict(format!(
                        "runtime execution {} belongs to task {}, not {}",
                        runtime_execution_id,
                        runtime_execution.task_kind.as_str(),
                        runtime_task_kind.as_str()
                    )));
                }
                Ok(())
            }
        }
    }

    async fn resolve_execution_scope(
        &self,
        state: &AppState,
        execution_kind: BillingExecutionOwnerKind,
        execution_id: Uuid,
    ) -> Result<BillingExecutionScope, ApiError> {
        let kind = execution_kind.as_str();
        let resolved = billing_repository::resolve_billing_execution_scope(
            &state.persistence.postgres,
            kind,
            execution_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| {
            ApiError::resource_not_found(
                billing_repository::billing_scope_not_found_resource(kind),
                execution_id,
            )
        })?;
        Ok(BillingExecutionScope {
            workspace_id: resolved.workspace_id,
            library_id: resolved.library_id,
            knowledge_document_id: resolved.knowledge_document_id,
        })
    }
}

fn invalid_execution_owner_kind(value: &str) -> ApiError {
    ApiError::BadRequest(format!("unsupported executionKind '{value}'"))
}

fn billing_rollup_pending(scope_kind: &str, scope_id: Uuid) -> ApiError {
    ApiError::service_unavailable(
        format!("billing cost for {scope_kind} {scope_id} is being reconciled"),
        "billing_rollup_pending",
    )
}

fn mixed_currency_billing_scope(scope_kind: &str, scope_id: Uuid) -> ApiError {
    ApiError::Conflict(format!(
        "{scope_kind} {scope_id} has billing costs in multiple currencies; request per-currency charges"
    ))
}

/// A library summary whose rollup is mid-reconciliation: the freshness marker
/// is the only factual value, every cost field is withheld as `null`.
fn reconciling_library_summary() -> LibraryCostSummary {
    LibraryCostSummary {
        cost_rollup_freshness: CostRollupFreshness::Reconciling,
        total_cost: None,
        currency_code: None,
        document_count: None,
        provider_call_count: None,
        unpriced_provider_call_count: None,
        cost_coverage: None,
    }
}

/// A workspace summary with the same withholding contract as
/// [`reconciling_library_summary`]; one dirty library defers the whole scope.
fn reconciling_workspace_summary() -> WorkspaceCostSummary {
    WorkspaceCostSummary {
        cost_rollup_freshness: CostRollupFreshness::Reconciling,
        total_cost: None,
        currency_code: None,
        library_count: None,
        document_count: None,
        provider_call_count: None,
        unpriced_provider_call_count: None,
        cost_coverage: None,
    }
}

pub(crate) fn ensure_billing_rollup_readable(
    scope_kind: &str,
    scope_id: Uuid,
    rollup_dirty: bool,
    terminal_error_code: Option<&str>,
) -> Result<(), ApiError> {
    if rollup_dirty {
        return Err(billing_rollup_pending(scope_kind, scope_id));
    }
    ensure_no_terminal_billing_rollup(scope_kind, scope_id, terminal_error_code)
}

/// Preserves the terminal rollup contract for reads that no longer fail closed
/// on a merely dirty generation: an unsupported terminal code is a 500 and
/// mixed currency is a 409. A dirty-but-not-terminal rollup is the caller's
/// business (it maps to `reconciling`).
pub(crate) fn ensure_no_terminal_billing_rollup(
    scope_kind: &str,
    scope_id: Uuid,
    terminal_error_code: Option<&str>,
) -> Result<(), ApiError> {
    match terminal_error_code {
        None => Ok(()),
        Some(MIXED_CURRENCY_TERMINAL_ERROR_CODE) => {
            Err(mixed_currency_billing_scope(scope_kind, scope_id))
        }
        Some(_) => Err(ApiError::InternalMessage(
            "billing cost has an unsupported terminal reconciliation state".to_string(),
        )),
    }
}

const fn execution_cost_rollup_error_is_retryable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Internal | ApiError::InternalMessage(_) | ApiError::ServiceUnavailable { .. }
    )
}

const fn provider_call_completion_error_is_retryable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Internal
            | ApiError::InternalMessage(_)
            | ApiError::ServiceUnavailable { .. }
            | ApiError::GatewayTimeout { .. }
    )
}

fn parse_execution_owner_kind(value: &str) -> Option<BillingExecutionOwnerKind> {
    match value {
        "query_execution" => Some(BillingExecutionOwnerKind::QueryExecution),
        "graph_extraction_attempt" => Some(BillingExecutionOwnerKind::GraphExtractionAttempt),
        "ingest_attempt" => Some(BillingExecutionOwnerKind::IngestAttempt),
        "mcp_document_read" => Some(BillingExecutionOwnerKind::McpDocumentRead),
        "knowledge_search" => Some(BillingExecutionOwnerKind::KnowledgeSearch),
        _ => None,
    }
}

fn map_provider_call_row(
    row: billing_repository::BillingProviderCallRow,
) -> Result<BillingProviderCall, String> {
    Ok(BillingProviderCall {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        binding_id: row.binding_id,
        owning_execution_kind: parse_execution_owner_kind(&row.owning_execution_kind)
            .ok_or_else(|| format!("unsupported execution kind '{}'", row.owning_execution_kind))?,
        owning_execution_id: row.owning_execution_id,
        runtime_execution_id: row.runtime_execution_id,
        runtime_task_kind: row
            .runtime_task_kind
            .as_deref()
            .map(str::parse::<RuntimeTaskKind>)
            .transpose()?,
        provider_catalog_id: row.provider_catalog_id,
        model_catalog_id: row.model_catalog_id,
        call_kind: row.call_kind,
        call_state: row.call_state,
        started_at: row.started_at,
        completed_at: row.completed_at,
    })
}

fn map_charge_row(row: billing_repository::BillingChargeRow) -> BillingCharge {
    BillingCharge {
        id: row.id,
        usage_id: row.usage_id,
        currency_code: row.currency_code,
        unit_price: row.unit_price,
        total_price: row.total_price,
        priced_at: row.priced_at,
    }
}

fn map_execution_cost_row(
    row: billing_repository::BillingExecutionCostRow,
) -> Result<BillingExecutionCost, String> {
    Ok(BillingExecutionCost {
        id: row.id,
        owning_execution_kind: parse_execution_owner_kind(&row.owning_execution_kind)
            .ok_or_else(|| format!("unsupported execution kind '{}'", row.owning_execution_kind))?,
        owning_execution_id: row.owning_execution_id,
        total_cost: row.total_cost,
        currency_code: row.currency_code,
        // The rollup mixes settled calls with unresolved reservations in its
        // unpriced count; derive coverage from the observations, not from a
        // settled-call denominator.
        cost_coverage: derived_cost_coverage(
            i64::from(row.unpriced_provider_call_count),
            row.total_cost,
        ),
        provider_call_count: row.provider_call_count,
        unpriced_provider_call_count: row.unpriced_provider_call_count,
        updated_at: row.updated_at,
    })
}

const fn execution_owner_kind_key(value: BillingExecutionOwnerKind) -> &'static str {
    value.as_str()
}

#[cfg(test)]
mod tests;
