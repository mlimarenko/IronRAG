mod provider_calls;
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
    infra::repositories::{
        self, ai_repository, billing_repository, catalog_repository, ingest_repository,
        query_repository, runtime_repository,
    },
    interfaces::http::router_support::ApiError,
    shared::database::error_is_unique_violation as database_error_is_unique_violation,
};

const EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS: [u64; 2] = [10, 50];
const PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS: [u64; 2] = [10, 50];
const MIXED_CURRENCY_TERMINAL_ERROR_CODE: &str = "mixed_currency";

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCostSummary {
    pub document_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i64,
    pub unpriced_provider_call_count: i64,
    pub cost_coverage: CostCoverageStatus,
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

impl CaptureIngestAttemptBillingCommand {
    pub(crate) fn for_chunk_embedding(
        workspace_id: Uuid,
        library_id: Uuid,
        attempt_id: Uuid,
        outcome: &crate::services::query::search::EmbedChunksStageOutcome,
    ) -> Option<Self> {
        let (Some(provider_kind), Some(model_name), Some(usage_json)) =
            (outcome.provider_kind.clone(), outcome.model_name.clone(), outcome.usage_json.clone())
        else {
            return None;
        };
        Some(Self {
            workspace_id,
            library_id,
            attempt_id,
            binding_id: None,
            provider_kind,
            model_name,
            call_kind: "embed_chunk".to_string(),
            usage_json,
        })
    }
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
}

fn provider_call_matches_reservation(
    existing: &billing_repository::BillingProviderCallRow,
    expected: &billing_repository::NewBillingProviderCall<'_>,
) -> bool {
    existing.id == expected.id
        && existing.workspace_id == expected.workspace_id
        && existing.library_id == expected.library_id
        && existing.binding_id == expected.binding_id
        && existing.owning_execution_kind == expected.owning_execution_kind
        && existing.owning_execution_id == expected.owning_execution_id
        && existing.runtime_execution_id == expected.runtime_execution_id
        && existing.runtime_task_kind.as_deref() == expected.runtime_task_kind
        && existing.provider_catalog_id == expected.provider_catalog_id
        && existing.model_catalog_id == expected.model_catalog_id
        && existing.call_kind == expected.call_kind
}

#[derive(Debug, Clone, Copy)]
struct BillingExecutionScope {
    workspace_id: Uuid,
    library_id: Uuid,
    knowledge_document_id: Option<Uuid>,
}

impl BillingService {
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
        match execution_kind {
            BillingExecutionOwnerKind::QueryExecution => {
                let execution = query_repository::get_execution_by_id(
                    &state.persistence.postgres,
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                if let Some(execution) = execution {
                    return Ok(BillingExecutionScope {
                        workspace_id: execution.workspace_id,
                        library_id: execution.library_id,
                        knowledge_document_id: None,
                    });
                }

                // MCP conversation retention intentionally cascades completed
                // query executions while preserving tracked provider-call
                // and charge history. A durable rollup cursor owns the
                // immutable workspace/library attribution needed to reconcile
                // that retained billing history. Falling back to it prevents
                // a migration-created dirty generation from retrying forever
                // and blocking every library cost read after its conversation
                // has been evicted.
                let retained_scope = billing_repository::get_execution_cost_rollup_state(
                    &state.persistence.postgres,
                    execution_owner_kind_key(execution_kind),
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
                tracing::debug!(
                    %execution_id,
                    workspace_id = %retained_scope.workspace_id,
                    library_id = %retained_scope.library_id,
                    "resolving retained query billing from its durable rollup scope",
                );
                Ok(BillingExecutionScope {
                    workspace_id: retained_scope.workspace_id,
                    library_id: retained_scope.library_id,
                    knowledge_document_id: None,
                })
            }
            BillingExecutionOwnerKind::GraphExtractionAttempt => {
                let extraction = repositories::runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
                    &state.persistence.postgres,
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("runtime_graph_extraction", execution_id)
                })?;
                let library = catalog_repository::get_library_by_id(
                    &state.persistence.postgres,
                    extraction.library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", extraction.library_id))?;
                Ok(BillingExecutionScope {
                    workspace_id: library.workspace_id,
                    library_id: extraction.library_id,
                    knowledge_document_id: Some(extraction.document_id),
                })
            }
            BillingExecutionOwnerKind::IngestAttempt => {
                let attempt = ingest_repository::get_ingest_attempt_by_id(
                    &state.persistence.postgres,
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", execution_id))?;
                let job = ingest_repository::get_ingest_job_by_id(
                    &state.persistence.postgres,
                    attempt.job_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("ingest_job", attempt.job_id))?;
                Ok(BillingExecutionScope {
                    workspace_id: job.workspace_id,
                    library_id: job.library_id,
                    knowledge_document_id: job.knowledge_document_id,
                })
            }
        }
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
        provider_call_count: row.provider_call_count,
        unpriced_provider_call_count: row.unpriced_provider_call_count,
        cost_coverage: CostCoverageStatus::from_call_counts(
            i64::from(row.provider_call_count),
            i64::from(row.unpriced_provider_call_count),
        ),
        updated_at: row.updated_at,
    })
}

const fn execution_owner_kind_key(value: BillingExecutionOwnerKind) -> &'static str {
    value.as_str()
}

#[cfg(test)]
mod tests;
