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
            PricingBillingUnit, PricingResolutionStatus,
        },
        provider_profiles::ProviderUsagePolicy,
    },
    infra::repositories::{
        self, ai_repository, billing_repository, catalog_repository, ingest_repository,
        query_repository, runtime_repository,
    },
    interfaces::http::router_support::ApiError,
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

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCostSummary {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCostSummary {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub library_count: i64,
    pub document_count: i64,
    pub provider_call_count: i64,
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

impl BillingService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Lists a keyset page of provider-call rows for a single execution,
    /// newest-first. `cursor` is the `(started_at, id)` of the last row of
    /// the previous page; `None` starts from the top.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository query fails or a persisted provider call cannot
    /// be mapped back into the canonical domain shape.
    pub async fn list_execution_provider_calls_page(
        &self,
        state: &AppState,
        execution_kind: BillingExecutionOwnerKind,
        execution_id: Uuid,
        cursor: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
        include_total: bool,
    ) -> Result<BillingSubResourcePage<BillingProviderCall>, ApiError> {
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let mut rows = billing_repository::list_provider_calls_by_execution_page(
            &state.persistence.postgres,
            execution_kind_key,
            execution_id,
            cursor,
            limit + 1,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let has_more = rows.len() as i64 > limit;
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let total = if include_total {
            Some(
                billing_repository::count_provider_calls_by_execution(
                    &state.persistence.postgres,
                    execution_kind_key,
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
            )
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(map_provider_call_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(ApiError::BadRequest)?;
        Ok(BillingSubResourcePage { items, has_more, total })
    }

    /// Lists a keyset page of billing charges for a single execution,
    /// newest-first. `cursor` is the `(priced_at, id)` of the last row of
    /// the previous page; `None` starts from the top.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository query fails.
    pub async fn list_execution_charges_page(
        &self,
        state: &AppState,
        execution_kind: BillingExecutionOwnerKind,
        execution_id: Uuid,
        cursor: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
        include_total: bool,
    ) -> Result<BillingSubResourcePage<BillingCharge>, ApiError> {
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let mut rows = billing_repository::list_charges_by_execution_page(
            &state.persistence.postgres,
            execution_kind_key,
            execution_id,
            cursor,
            limit + 1,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let has_more = rows.len() as i64 > limit;
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let total = if include_total {
            Some(
                billing_repository::count_charges_by_execution(
                    &state.persistence.postgres,
                    execution_kind_key,
                    execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
            )
        } else {
            None
        };
        Ok(BillingSubResourcePage {
            items: rows.into_iter().map(map_charge_row).collect(),
            has_more,
            total,
        })
    }

    /// Loads the rolled-up billing cost for a single execution.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid, the repository query fails, or a
    /// persisted billing row cannot be mapped back into the canonical domain shape.
    pub async fn get_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<BillingExecutionCost, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?;
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let snapshot = billing_repository::get_execution_cost_read_snapshot(
            &state.persistence.postgres,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_no_terminal_billing_rollup(
            "execution",
            execution_id,
            snapshot.terminal_error_code.as_deref(),
        )?;
        let canonical_aggregate_count =
            i32::try_from(snapshot.canonical_provider_call_count).unwrap_or(i32::MAX);
        let aggregate_missing_or_mismatched = snapshot.execution_cost.as_ref().map_or_else(
            || snapshot.canonical_provider_call_count > 0,
            |cost| {
                snapshot.canonical_provider_call_count == 0
                    || cost.provider_call_count != canonical_aggregate_count
            },
        );
        let untracked_canonical_cost = !snapshot.rollup_state_present
            && (snapshot.canonical_provider_call_count > 0 || snapshot.execution_cost.is_some());
        if snapshot.rollup_dirty || untracked_canonical_cost || aggregate_missing_or_mismatched {
            // Dirty generations, missing durable cursors, and defensive count
            // mismatches repair under the same advisory lock used by writers.
            // Clean, matching rows no longer pay for an unconditional
            // aggregate rebuild on every GET.
            return self
                .roll_up_execution_cost_with_retry(state, execution_kind_key, execution_id)
                .await?
                .ok_or_else(|| {
                    ApiError::resource_not_found("billing_execution_cost", execution_id)
                });
        }

        if let Some(row) = snapshot.execution_cost {
            return map_execution_cost_row(row).map_err(ApiError::BadRequest);
        }

        // Some executions are legitimately zero-cost (no billable provider call captured).
        // Expose deterministic zero-cost truth instead of surfacing an ambiguous 404.
        Ok(BillingExecutionCost {
            id: Uuid::now_v7(),
            owning_execution_kind: execution_kind,
            owning_execution_id: execution_id,
            total_cost: Decimal::ZERO,
            currency_code: "USD".to_string(),
            provider_call_count: 0,
            updated_at: Utc::now(),
        })
    }

    /// Lists document-level cost summaries for a library.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn list_document_costs_for_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<DocumentCostSummary>, ApiError> {
        let snapshot = billing_repository::get_document_costs_read_snapshot(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_billing_rollup_readable(
            "library",
            library_id,
            snapshot.rollup_dirty,
            snapshot.terminal_error_code.as_deref(),
        )?;
        let mut seen_documents = HashSet::with_capacity(snapshot.rows.len());
        snapshot
            .rows
            .into_iter()
            .map(|r| {
                if !seen_documents.insert(r.document_id) {
                    return Err(mixed_currency_billing_scope("document", r.document_id));
                }
                Ok(DocumentCostSummary {
                    document_id: r.document_id,
                    total_cost: r.total_cost,
                    currency_code: r.currency_code,
                    provider_call_count: r.provider_call_count,
                })
            })
            .collect()
    }

    /// Lists an offset page of document-level cost summaries for a library.
    ///
    /// The underlying repository read is one atomic rollup-health snapshot
    /// (see [`list_document_costs_for_library`](Self::list_document_costs_for_library)); pagination
    /// is applied in-memory over that already-materialized, already-ordered
    /// result rather than pushed into the snapshot query, so the
    /// rollup-dirty/mixed-currency guarantees it provides are untouched.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn list_document_costs_for_library_page(
        &self,
        state: &AppState,
        library_id: Uuid,
        offset: usize,
        limit: usize,
        include_total: bool,
    ) -> Result<BillingSubResourcePage<DocumentCostSummary>, ApiError> {
        let all = self.list_document_costs_for_library(state, library_id).await?;
        let total = include_total.then_some(all.len() as i64);
        let has_more = offset.saturating_add(limit) < all.len();
        let items = all.into_iter().skip(offset).take(limit).collect();
        Ok(BillingSubResourcePage { items, has_more, total })
    }

    /// Loads the rolled-up cost summary for a library.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn get_library_cost_summary(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<LibraryCostSummary, ApiError> {
        let snapshot = billing_repository::get_library_cost_read_snapshot(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_billing_rollup_readable(
            "library",
            library_id,
            snapshot.rollup_dirty,
            snapshot.terminal_error_code.as_deref(),
        )?;
        match snapshot.rows.as_slice() {
            [r] => Ok(LibraryCostSummary {
                total_cost: r.total_cost,
                currency_code: r.currency_code.clone(),
                document_count: r.document_count,
                provider_call_count: r.provider_call_count,
            }),
            [] => Ok(LibraryCostSummary {
                total_cost: Decimal::ZERO,
                currency_code: "USD".to_string(),
                document_count: 0,
                provider_call_count: 0,
            }),
            _ => Err(mixed_currency_billing_scope("library", library_id)),
        }
    }

    /// Loads the rolled-up cost summary for a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn get_workspace_cost_summary(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<WorkspaceCostSummary, ApiError> {
        let snapshot = billing_repository::get_workspace_cost_read_snapshot(
            &state.persistence.postgres,
            workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_billing_rollup_readable(
            "workspace",
            workspace_id,
            snapshot.rollup_dirty,
            snapshot.terminal_error_code.as_deref(),
        )?;
        match snapshot.rows.as_slice() {
            [r] => Ok(WorkspaceCostSummary {
                total_cost: r.total_cost,
                currency_code: r.currency_code.clone(),
                library_count: r.library_count,
                document_count: r.document_count,
                provider_call_count: r.provider_call_count,
            }),
            [] => Ok(WorkspaceCostSummary {
                total_cost: Decimal::ZERO,
                currency_code: "USD".to_string(),
                library_count: 0,
                document_count: 0,
                provider_call_count: 0,
            }),
            _ => Err(mixed_currency_billing_scope("workspace", workspace_id)),
        }
    }

    /// Resolves the library that owns a billing execution.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid or the execution scope cannot be
    /// resolved.
    pub async fn resolve_execution_library_id(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let scope = self
            .resolve_execution_scope(
                state,
                parse_execution_owner_kind(execution_kind)
                    .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?,
                execution_id,
            )
            .await?;
        Ok(scope.library_id)
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

    /// Creates a durable `started` provider-call row before a paid request is
    /// issued. Callers must complete or fail the returned reservation.
    ///
    /// # Errors
    /// Returns an [`ApiError`] when attribution is invalid or the reservation
    /// cannot be persisted. A caller must not issue the provider request after
    /// an error.
    pub async fn reserve_execution_provider_call(
        &self,
        state: &AppState,
        command: ReserveExecutionProviderCallCommand,
    ) -> Result<Uuid, ApiError> {
        self.reserve_execution_provider_call_with_id(state, Uuid::now_v7(), command).await
    }

    /// Persists a reservation using a caller-known ID. Optional latency-bound
    /// stages use this form so an ambiguous timeout can always be reconciled
    /// without losing ownership of a committed `started` row.
    pub async fn reserve_execution_provider_call_with_id(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        command: ReserveExecutionProviderCallCommand,
    ) -> Result<Uuid, ApiError> {
        let owning_execution_kind = parse_execution_owner_kind(&command.owning_execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(&command.owning_execution_kind))?;
        self.validate_runtime_attribution(
            state,
            owning_execution_kind,
            command.owning_execution_id,
            command.runtime_execution_id,
            command.runtime_task_kind,
        )
        .await?;
        let execution_scope = self
            .resolve_execution_scope(state, owning_execution_kind, command.owning_execution_id)
            .await?;
        if execution_scope.workspace_id != command.workspace_id
            || execution_scope.library_id != command.library_id
        {
            return Err(ApiError::Conflict(format!(
                "provider call attribution does not match execution {} scope",
                command.owning_execution_id
            )));
        }

        let new_provider_call = billing_repository::NewBillingProviderCall {
            id: provider_call_id,
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            binding_id: command.binding_id,
            owning_execution_kind: execution_owner_kind_key(owning_execution_kind),
            owning_execution_id: command.owning_execution_id,
            runtime_execution_id: command.runtime_execution_id,
            runtime_task_kind: command.runtime_task_kind.map(RuntimeTaskKind::as_str),
            provider_catalog_id: command.provider_catalog_id,
            model_catalog_id: command.model_catalog_id,
            call_kind: &command.call_kind,
            call_state: "started",
            completed_at: None,
        };
        let provider_call = match billing_repository::create_provider_call(
            &state.persistence.postgres,
            &new_provider_call,
        )
        .await
        {
            Ok(provider_call) => provider_call,
            Err(error) if database_error_is_unique_violation(&error) => {
                let existing = billing_repository::get_provider_call_by_id(
                    &state.persistence.postgres,
                    provider_call_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::Conflict(format!(
                        "provider call {provider_call_id} reservation conflict disappeared"
                    ))
                })?;
                if !provider_call_matches_reservation(&existing, &new_provider_call)
                    || existing.call_state != "started"
                {
                    return Err(ApiError::Conflict(format!(
                        "provider call {provider_call_id} already belongs to a different or terminal event"
                    )));
                }
                existing
            }
            Err(error) => return Err(ApiError::internal_with_log(error, "internal")),
        };
        Ok(provider_call.id)
    }

    /// Cancels a known reservation when it exists, and is a no-op when an
    /// interrupted INSERT never committed. This is intended for asynchronous
    /// reconciliation after a deadline or task cancellation.
    pub async fn cancel_reserved_provider_call_if_present(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
    ) -> Result<bool, ApiError> {
        let exists = billing_repository::get_provider_call_by_id(
            &state.persistence.postgres,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .is_some();
        if !exists {
            return Ok(false);
        }
        self.finish_reserved_provider_call_without_usage(state, provider_call_id, "canceled")
            .await?;
        Ok(true)
    }

    /// Atomically attaches provider-reported usage and charges to a durable
    /// reservation and marks it completed. Any pre-commit error or cancellation
    /// rolls the whole transaction back to a clean, retryable `started` row.
    pub async fn complete_reserved_provider_call(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let provider_call = self
            .persist_reserved_provider_call_completion(state, provider_call_id, usage_json)
            .await?;
        match self
            .roll_up_execution_cost_with_retry(
                state,
                &provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
            )
            .await
        {
            Ok(rollup) => Ok(rollup),
            Err(error) => {
                // Usage, charges, and the completed state are canonical and
                // already committed. The derived row received bounded retries;
                // do not turn a valid provider response into a query fallback
                // after the point of commit.
                tracing::error!(
                    %provider_call_id,
                    owning_execution_kind = %provider_call.owning_execution_kind,
                    owning_execution_id = %provider_call.owning_execution_id,
                    %error,
                    "provider call completed but bounded execution-cost rollup repair failed"
                );
                Ok(None)
            }
        }
    }

    /// Completes canonical usage accounting, then repairs the derived cost
    /// rollup off the latency-sensitive request path.
    ///
    /// The canonical completion transaction also increments a durable rollup
    /// generation. Reads fail closed while it is dirty and the bounded worker
    /// repair sweep retries it after crashes. The detached task below is only
    /// a latency optimization, never the correctness mechanism, so optional
    /// query stages do not wait for derived aggregate work.
    pub async fn complete_reserved_provider_call_deferred_rollup(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let provider_call = self
            .persist_reserved_provider_call_completion(state, provider_call_id, usage_json)
            .await?;
        let service = self.clone();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .roll_up_execution_cost_with_retry(
                    &state,
                    &provider_call.owning_execution_kind,
                    provider_call.owning_execution_id,
                )
                .await
            {
                tracing::error!(
                    %provider_call_id,
                    owning_execution_kind = %provider_call.owning_execution_kind,
                    owning_execution_id = %provider_call.owning_execution_id,
                    %error,
                    "deferred execution-cost rollup repair failed after canonical provider completion"
                );
            }
        });
        Ok(())
    }

    /// Completes one caller-stable provider event with bounded local retries.
    ///
    /// The canonical completion transaction is idempotent by provider-call ID,
    /// so a retry after a pre-commit failure or an ambiguous commit
    /// acknowledgement cannot duplicate usage or charges. This protects
    /// in-process response handling only; it does not claim provider-side
    /// exactly-once execution or survive loss of the known usage payload in a
    /// process crash.
    ///
    /// # Errors
    ///
    /// Returns the final non-retryable error or the last retryable error after
    /// the bounded retry budget is exhausted.
    pub async fn complete_reserved_provider_call_deferred_rollup_with_retry(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let mut retry_index = 0usize;
        loop {
            match self
                .complete_reserved_provider_call_deferred_rollup(
                    state,
                    provider_call_id,
                    usage_json,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(error)
                    if provider_call_completion_error_is_retryable(&error)
                        && retry_index < PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS.len() =>
                {
                    let delay_ms = PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS[retry_index];
                    retry_index += 1;
                    tracing::warn!(
                        %provider_call_id,
                        attempt = retry_index + 1,
                        delay_ms,
                        %error,
                        "retrying canonical provider-call completion"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn persist_reserved_provider_call_completion(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<billing_repository::BillingProviderCallRow, ApiError> {
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // Serialize completion attempts and keep usage, charges, and the state
        // transition in one transaction. On any error or task cancellation,
        // dropping the transaction rolls every partial insert back, leaving a
        // clean `started` reservation that is safe for an operator retry.
        let provider_call = billing_repository::get_provider_call_by_id_for_update(
            &mut *transaction,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("billing_provider_call", provider_call_id))?;

        // Retrying the same stable local event after an ambiguous commit must
        // never duplicate usage or charges. A completed row is the durable
        // acknowledgement for the original completion attempt.
        if provider_call.call_state == "completed" {
            transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            return Ok(provider_call);
        }
        if provider_call.call_state != "started" {
            return Err(ApiError::Conflict(format!(
                "provider call {provider_call_id} is already {}",
                provider_call.call_state
            )));
        }

        self.persist_provider_call_usage_in_transaction(
            &mut transaction,
            &provider_call,
            usage_json,
        )
        .await?;
        billing_repository::update_provider_call_state(
            &mut *transaction,
            provider_call_id,
            "completed",
            Some(Utc::now()),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| {
            ApiError::Conflict(format!(
                "provider call {provider_call_id} was finalized concurrently"
            ))
        })?;
        billing_repository::mark_execution_cost_rollup_dirty(&mut *transaction, &provider_call)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(provider_call)
    }

    /// Marks a reserved call failed or canceled when no response usage exists.
    pub async fn finish_reserved_provider_call_without_usage(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        call_state: &str,
    ) -> Result<(), ApiError> {
        if !matches!(call_state, "failed" | "canceled") {
            return Err(ApiError::BadRequest(
                "reserved provider call terminal state must be failed or canceled".to_string(),
            ));
        }
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let existing = billing_repository::get_provider_call_by_id_for_update(
            &mut *transaction,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("billing_provider_call", provider_call_id))?;
        let (provider_call, transitioned) = if existing.call_state == "started" {
            let provider_call = billing_repository::update_provider_call_state(
                &mut *transaction,
                provider_call_id,
                call_state,
                Some(Utc::now()),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "provider call {provider_call_id} could not be terminalized"
                ))
            })?;
            (provider_call, true)
        } else {
            // A concurrent completion/failure already won. Preserve its
            // terminal state rather than relabeling a paid call.
            (existing, false)
        };
        if transitioned {
            billing_repository::mark_execution_cost_rollup_dirty(&mut *transaction, &provider_call)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Err(error) = self
            .roll_up_execution_cost_with_retry(
                state,
                &provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
            )
            .await
        {
            tracing::error!(
                %provider_call_id,
                owning_execution_kind = %provider_call.owning_execution_kind,
                owning_execution_id = %provider_call.owning_execution_id,
                %error,
                "terminal provider call persisted but bounded execution-cost rollup repair failed"
            );
        }
        Ok(())
    }

    /// Captures a canonical provider-call record and rolls its execution cost forward.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when attribution fails, repository writes fail, or required billing
    /// metadata cannot be resolved.
    pub async fn capture_execution_provider_call(
        &self,
        state: &AppState,
        command: CaptureExecutionBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let owning_execution_kind = parse_execution_owner_kind(&command.owning_execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(&command.owning_execution_kind))?;
        self.validate_runtime_attribution(
            state,
            owning_execution_kind,
            command.owning_execution_id,
            command.runtime_execution_id,
            command.runtime_task_kind,
        )
        .await?;
        let execution_scope = self
            .resolve_execution_scope(state, owning_execution_kind, command.owning_execution_id)
            .await?;
        if execution_scope.workspace_id != command.workspace_id {
            return Err(ApiError::Conflict(format!(
                "execution {} belongs to workspace {}, not {}",
                command.owning_execution_id, execution_scope.workspace_id, command.workspace_id
            )));
        }
        if execution_scope.library_id != command.library_id {
            return Err(ApiError::Conflict(format!(
                "execution {} belongs to library {}, not {}",
                command.owning_execution_id, execution_scope.library_id, command.library_id
            )));
        }
        let Some(provider_catalog) = ai_repository::get_provider_catalog_by_kind(
            &state.persistence.postgres,
            &command.provider_kind,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        let model_capability_kind = billing_model_capability_kind(&command.call_kind);
        let Some(model_catalog) = ai_repository::get_model_catalog_by_provider_name_and_capability(
            &state.persistence.postgres,
            &command.provider_kind,
            &command.model_name,
            model_capability_kind,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };

        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let provider_call = billing_repository::create_provider_call(
            &mut *transaction,
            &billing_repository::NewBillingProviderCall {
                id: Uuid::now_v7(),
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                binding_id: command.binding_id,
                owning_execution_kind: execution_owner_kind_key(owning_execution_kind),
                owning_execution_id: command.owning_execution_id,
                runtime_execution_id: command.runtime_execution_id,
                runtime_task_kind: command.runtime_task_kind.map(RuntimeTaskKind::as_str),
                provider_catalog_id: provider_catalog.id,
                model_catalog_id: model_catalog.id,
                call_kind: &command.call_kind,
                call_state: "completed",
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        self.persist_provider_call_usage_in_transaction(
            &mut transaction,
            &provider_call,
            &command.usage_json,
        )
        .await?;
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        self.roll_up_execution_cost_with_retry(
            state,
            execution_owner_kind_key(owning_execution_kind),
            command.owning_execution_id,
        )
        .await
    }

    async fn persist_provider_call_usage_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        provider_call: &billing_repository::BillingProviderCallRow,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let provider_profile = ai_repository::get_provider_billing_usage_profile(
            &mut **transaction,
            provider_call.provider_catalog_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let configured_semantics = if let Some(profile) = provider_profile.as_ref() {
            let policy = parse_provider_usage_policy(profile.usage_policy_json.as_ref()).map_err(
                |error| {
                    ApiError::internal_with_log(
                        error,
                        "invalid typed provider usage policy in catalog",
                    )
                },
            )?;
            configured_provider_usage_semantics(policy)
        } else {
            // The provider-call foreign key should make this impossible.
            // Keep usage durable if the catalog is nevertheless damaged,
            // and use formal payload shape instead of guessing from an
            // identifier.
            tracing::error!(
                provider_call_id = %provider_call.id,
                provider_catalog_id = %provider_call.provider_catalog_id,
                "billing provider usage profile is missing"
            );
            ProviderUsageSemantics::AutoDetect
        };
        let usage_semantics =
            provider_usage_semantics_for_payload(usage_json, configured_semantics);
        let normalized_usage = normalize_token_usage(usage_json, usage_semantics);
        if normalized_usage.cached_input_was_clamped {
            tracing::warn!(
                provider_call_id = %provider_call.id,
                "provider reported more cached input tokens than total input tokens; billing quantity was clamped"
            );
        }
        let request_input_tokens = normalized_usage.context_input_tokens.and_then(decimal_to_i32);
        let price_variant_key = extract_price_variant_key(usage_json);
        let usages = normalized_usage.rows(provider_call.id);
        for usage in usages {
            let usage_row = billing_repository::create_usage(&mut **transaction, &usage)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let Some(price) = ai_repository::get_effective_price_catalog_entry(
                &mut **transaction,
                provider_call.model_catalog_id,
                &usage_row.billing_unit,
                Some(provider_call.workspace_id),
                usage_row.observed_at,
                &price_variant_key,
                request_input_tokens,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            else {
                let pricing_status = PricingResolutionStatus::PricingMissing;
                tracing::warn!(
                    provider_call_id = %provider_call.id,
                    usage_id = %usage_row.id,
                    billing_unit = %usage_row.billing_unit,
                    pricing_status = pricing_status.as_str(),
                    "billing usage was recorded without an explicit catalog price"
                );
                continue;
            };

            let total_price = price.unit_price * usage_row.quantity / Decimal::from(1_000_000u64);
            let _ = billing_repository::create_charge(
                &mut **transaction,
                &billing_repository::NewBillingCharge {
                    usage_id: usage_row.id,
                    price_catalog_id: price.id,
                    currency_code: price.currency_code,
                    unit_price: price.unit_price,
                    total_price,
                    priced_at: Some(Utc::now()),
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        Ok(())
    }

    /// Recomputes the rolled-up billing cost for an execution after provider usage changes.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid or repository writes fail.
    pub async fn roll_up_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?;
        // Resolve canonical execution scope (library + document) so the
        // rollup row carries its own attribution columns. Both billing
        // read endpoints (/library-cost-summary and /library-document-costs)
        // read those columns directly without re-joining provider_call. Do
        // this before opening the single-connection transaction below.
        let scope = self.resolve_execution_scope(state, execution_kind, execution_id).await?;
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        billing_repository::lock_execution_cost_rollup(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let rollup_state = billing_repository::ensure_execution_cost_rollup_state(
            &mut *transaction,
            execution_kind_key,
            execution_id,
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let provider_call_count = billing_repository::count_provider_calls_by_execution(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let rollups = billing_repository::list_execution_cost_rollups(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let (total_cost, currency_code) = match rollups.as_slice() {
            [] => (Decimal::ZERO, "USD".to_string()),
            [rollup] => (rollup.total_cost, rollup.currency_code.clone()),
            _ => {
                let terminal_acknowledged =
                    billing_repository::acknowledge_execution_cost_rollup_terminal_error(
                        &mut *transaction,
                        execution_kind_key,
                        execution_id,
                        rollup_state.dirty_generation,
                        MIXED_CURRENCY_TERMINAL_ERROR_CODE,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                if !terminal_acknowledged {
                    return Err(ApiError::service_unavailable(
                        "billing execution changed while its cost was being reconciled",
                        "billing_rollup_raced",
                    ));
                }
                transaction
                    .commit()
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                return Err(mixed_currency_billing_scope("execution", execution_id));
            }
        };
        let provider_call_count = i32::try_from(provider_call_count).unwrap_or(i32::MAX);
        let row = billing_repository::upsert_execution_cost(
            &mut *transaction,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: execution_kind_key,
                owning_execution_id: execution_id,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                knowledge_document_id: scope.knowledge_document_id,
                total_cost,
                currency_code: &currency_code,
                provider_call_count,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let generation_acknowledged =
            billing_repository::acknowledge_execution_cost_rollup_generation(
                &mut *transaction,
                execution_kind_key,
                execution_id,
                rollup_state.dirty_generation,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !generation_acknowledged {
            return Err(ApiError::service_unavailable(
                "billing execution changed while its cost was being reconciled",
                "billing_rollup_raced",
            ));
        }
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(Some(map_execution_cost_row(row).map_err(ApiError::BadRequest)?))
    }

    async fn roll_up_execution_cost_with_retry(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let mut retry_index = 0_usize;
        loop {
            match self.roll_up_execution_cost(state, execution_kind, execution_id).await {
                Ok(rollup) => return Ok(rollup),
                Err(error)
                    if execution_cost_rollup_error_is_retryable(&error)
                        && retry_index < EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS.len() =>
                {
                    let delay_ms = EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS[retry_index];
                    retry_index += 1;
                    tracing::warn!(
                        execution_kind,
                        %execution_id,
                        attempt = retry_index,
                        delay_ms,
                        %error,
                        "retrying derived execution-cost rollup"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn database_error_is_unique_violation(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation)
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
                // query executions while preserving canonical provider-call
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
                let extraction = repositories::get_runtime_graph_extraction_record_by_id(
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

fn ensure_no_terminal_billing_rollup(
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProviderUsageSemantics {
    AutoDetect,
    CachedSubsetOfInput,
    DisjointCacheCounters,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct NormalizedTokenUsage {
    ordinary_input_tokens: Option<Decimal>,
    output_tokens: Option<Decimal>,
    cached_input_tokens: Option<Decimal>,
    cache_creation_input_tokens: Option<Decimal>,
    /// Total request input used only to select a context-size price tier.
    context_input_tokens: Option<Decimal>,
    cached_input_was_clamped: bool,
}

impl NormalizedTokenUsage {
    fn rows(&self, provider_call_id: Uuid) -> Vec<billing_repository::NewBillingUsage<'static>> {
        let mut rows = Vec::with_capacity(4);
        if let Some(quantity) = self.ordinary_input_tokens {
            rows.push(billing_repository::NewBillingUsage {
                provider_call_id,
                usage_kind: "prompt_tokens",
                billing_unit: PricingBillingUnit::Per1MInputTokens.as_str(),
                quantity,
                observed_at: Some(Utc::now()),
            });
        }
        if let Some(quantity) = self.cache_creation_input_tokens {
            rows.push(billing_repository::NewBillingUsage {
                provider_call_id,
                usage_kind: "cache_creation_input_tokens",
                billing_unit: PricingBillingUnit::Per1MCacheWriteInputTokens.as_str(),
                quantity,
                observed_at: Some(Utc::now()),
            });
        }
        if let Some(quantity) = self.output_tokens {
            rows.push(billing_repository::NewBillingUsage {
                provider_call_id,
                usage_kind: "completion_tokens",
                billing_unit: PricingBillingUnit::Per1MOutputTokens.as_str(),
                quantity,
                observed_at: Some(Utc::now()),
            });
        }
        if let Some(quantity) = self.cached_input_tokens {
            rows.push(billing_repository::NewBillingUsage {
                provider_call_id,
                usage_kind: "cached_input_tokens",
                billing_unit: PricingBillingUnit::Per1MCachedInputTokens.as_str(),
                quantity,
                observed_at: Some(Utc::now()),
            });
        }
        rows
    }
}

fn parse_provider_usage_policy(
    value: Option<&Value>,
) -> Result<ProviderUsagePolicy, serde_json::Error> {
    match value {
        Some(value) => serde_json::from_value(value.clone()),
        None => Ok(ProviderUsagePolicy::default()),
    }
}

const fn configured_provider_usage_semantics(
    policy: ProviderUsagePolicy,
) -> ProviderUsageSemantics {
    match policy {
        ProviderUsagePolicy::AutoDetect => ProviderUsageSemantics::AutoDetect,
        ProviderUsagePolicy::CachedSubsetOfInput => ProviderUsageSemantics::CachedSubsetOfInput,
        ProviderUsagePolicy::DisjointCacheCounters => ProviderUsageSemantics::DisjointCacheCounters,
    }
}

fn provider_usage_semantics_for_payload(
    usage_json: &Value,
    configured: ProviderUsageSemantics,
) -> ProviderUsageSemantics {
    if configured != ProviderUsageSemantics::AutoDetect {
        return configured;
    }

    // Formal disjoint-cache counters are a protocol-level signal.
    if usage_json.get("cache_creation_input_tokens").is_some()
        || usage_json.get("cache_read_input_tokens").is_some()
    {
        return ProviderUsageSemantics::DisjointCacheCounters;
    }
    // Nested cached-token counters identify subset accounting.
    if usage_json
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .is_some()
        || usage_json
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .is_some()
    {
        return ProviderUsageSemantics::CachedSubsetOfInput;
    }
    ProviderUsageSemantics::CachedSubsetOfInput
}

fn normalize_token_usage(
    usage_json: &Value,
    semantics: ProviderUsageSemantics,
) -> NormalizedTokenUsage {
    let output_tokens = parse_usage_quantity(usage_json, &["completion_tokens", "output_tokens"]);
    match semantics {
        ProviderUsageSemantics::DisjointCacheCounters => {
            let base_input_tokens =
                parse_usage_quantity(usage_json, &["input_tokens", "prompt_tokens"]);
            let cache_creation_tokens =
                parse_usage_quantity(usage_json, &["cache_creation_input_tokens"]);
            let ordinary_input_tokens = base_input_tokens;
            let cached_input_tokens = parse_usage_quantity(
                usage_json,
                &["cache_read_input_tokens", "cached_input_tokens", "input_cached_tokens"],
            );
            let context_input_tokens = sum_positive_quantities([
                ordinary_input_tokens,
                cache_creation_tokens,
                cached_input_tokens,
            ]);
            NormalizedTokenUsage {
                ordinary_input_tokens,
                output_tokens,
                cached_input_tokens,
                cache_creation_input_tokens: cache_creation_tokens,
                context_input_tokens,
                cached_input_was_clamped: false,
            }
        }
        ProviderUsageSemantics::CachedSubsetOfInput | ProviderUsageSemantics::AutoDetect => {
            let total_input_tokens =
                parse_usage_quantity(usage_json, &["prompt_tokens", "input_tokens"]);
            let reported_cached_input_tokens = parse_subset_cached_input_quantity(usage_json);
            let (ordinary_input_tokens, cached_input_tokens, cached_input_was_clamped) =
                match (total_input_tokens, reported_cached_input_tokens) {
                    (Some(total), Some(cached)) => {
                        let effective_cached = if cached > total { total } else { cached };
                        (
                            positive_quantity(total - effective_cached),
                            positive_quantity(effective_cached),
                            cached > total,
                        )
                    }
                    (Some(total), None) => (Some(total), None, false),
                    (None, Some(cached)) => (None, Some(cached), false),
                    (None, None) => (None, None, false),
                };
            let context_input_tokens = total_input_tokens
                .or_else(|| sum_positive_quantities([ordinary_input_tokens, cached_input_tokens]));
            NormalizedTokenUsage {
                ordinary_input_tokens,
                output_tokens,
                cached_input_tokens,
                cache_creation_input_tokens: None,
                context_input_tokens,
                cached_input_was_clamped,
            }
        }
    }
}

fn parse_usage_quantity(usage_json: &Value, keys: &[&str]) -> Option<Decimal> {
    keys.iter().filter_map(|key| usage_json.get(*key)).find_map(|value| {
        let quantity = match value {
            Value::Number(number) => {
                number.as_i64().map(Decimal::from).or_else(|| number.as_u64().map(Decimal::from))
            }
            Value::String(text) => text.parse::<i64>().ok().map(Decimal::from),
            _ => None,
        };
        quantity.filter(|quantity| *quantity > Decimal::ZERO)
    })
}

fn parse_subset_cached_input_quantity(usage_json: &Value) -> Option<Decimal> {
    usage_json
        .get("prompt_tokens_details")
        .and_then(|details| parse_usage_quantity(details, &["cached_tokens"]))
        .or_else(|| {
            usage_json
                .get("input_tokens_details")
                .and_then(|details| parse_usage_quantity(details, &["cached_tokens"]))
        })
        .or_else(|| {
            parse_usage_quantity(usage_json, &["cached_input_tokens", "input_cached_tokens"])
        })
}

fn positive_quantity(value: Decimal) -> Option<Decimal> {
    (value > Decimal::ZERO).then_some(value)
}

fn sum_positive_quantities<const N: usize>(values: [Option<Decimal>; N]) -> Option<Decimal> {
    positive_quantity(values.into_iter().flatten().sum())
}

fn decimal_to_i32(value: Decimal) -> Option<i32> {
    value.round().to_i32()
}

fn extract_price_variant_key(usage_json: &Value) -> String {
    usage_json
        .get("price_variant_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn billing_model_capability_kind(call_kind: &str) -> &'static str {
    match call_kind {
        "embed_chunk" | "query_embedding" => "embedding",
        _ => "chat",
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
        price_catalog_id: row.price_catalog_id,
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
        updated_at: row.updated_at,
    })
}

const fn execution_owner_kind_key(value: BillingExecutionOwnerKind) -> &'static str {
    value.as_str()
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderUsageSemantics, billing_model_capability_kind, configured_provider_usage_semantics,
        execution_cost_rollup_error_is_retryable, normalize_token_usage,
        provider_usage_semantics_for_payload,
    };
    use crate::domains::{
        billing::{PricingBillingUnit, PricingResolutionStatus},
        provider_profiles::ProviderUsagePolicy,
    };
    use crate::interfaces::http::router_support::ApiError;
    use rust_decimal::Decimal;

    fn quantity_for(usage: &super::NormalizedTokenUsage, usage_kind: &str) -> Option<Decimal> {
        usage
            .rows(uuid::Uuid::nil())
            .into_iter()
            .find(|row| row.usage_kind == usage_kind)
            .map(|row| row.quantity)
    }

    fn billing_unit_for<'a>(
        usage: &'a super::NormalizedTokenUsage,
        usage_kind: &str,
    ) -> Option<&'a str> {
        usage
            .rows(uuid::Uuid::nil())
            .into_iter()
            .find(|row| row.usage_kind == usage_kind)
            .map(|row| row.billing_unit)
    }

    #[test]
    fn cached_subset_is_subtracted_from_total_input_before_pricing() {
        let usage = normalize_token_usage(
            &serde_json::json!({
                "prompt_tokens": 1000,
                "completion_tokens": 100,
                "prompt_tokens_details": {"cached_tokens": 400}
            }),
            ProviderUsageSemantics::CachedSubsetOfInput,
        );

        assert_eq!(quantity_for(&usage, "prompt_tokens"), Some(Decimal::from(600)));
        assert_eq!(quantity_for(&usage, "cached_input_tokens"), Some(Decimal::from(400)));
        assert_eq!(quantity_for(&usage, "completion_tokens"), Some(Decimal::from(100)));
        assert_eq!(usage.context_input_tokens, Some(Decimal::from(1000)));
        assert!(!usage.cached_input_was_clamped);
    }

    #[test]
    fn disjoint_cache_read_and_creation_counters_are_preserved() {
        let usage = normalize_token_usage(
            &serde_json::json!({
                "input_tokens": 600,
                "output_tokens": 100,
                "cache_creation_input_tokens": 200,
                "cache_read_input_tokens": 400
            }),
            ProviderUsageSemantics::DisjointCacheCounters,
        );

        assert_eq!(quantity_for(&usage, "prompt_tokens"), Some(Decimal::from(600)));
        assert_eq!(quantity_for(&usage, "cache_creation_input_tokens"), Some(Decimal::from(200)));
        assert_eq!(
            billing_unit_for(&usage, "cache_creation_input_tokens"),
            Some("per_1m_cache_write_input_tokens")
        );
        assert_eq!(quantity_for(&usage, "cached_input_tokens"), Some(Decimal::from(400)));
        assert_eq!(quantity_for(&usage, "completion_tokens"), Some(Decimal::from(100)));
        assert_eq!(usage.context_input_tokens, Some(Decimal::from(1200)));
    }

    #[test]
    fn cache_write_billing_unit_and_missing_price_status_have_stable_keys() {
        assert_eq!(
            PricingBillingUnit::Per1MCacheWriteInputTokens.as_str(),
            "per_1m_cache_write_input_tokens"
        );
        assert_eq!(PricingResolutionStatus::PricingMissing.as_str(), "pricing_missing");
    }

    #[test]
    fn no_cache_usage_preserves_the_full_input_quantity() {
        let usage = normalize_token_usage(
            &serde_json::json!({"input_tokens": 750, "output_tokens": 25}),
            ProviderUsageSemantics::CachedSubsetOfInput,
        );

        assert_eq!(quantity_for(&usage, "prompt_tokens"), Some(Decimal::from(750)));
        assert_eq!(quantity_for(&usage, "cached_input_tokens"), None);
        assert_eq!(usage.context_input_tokens, Some(Decimal::from(750)));
    }

    #[test]
    fn overreported_cached_subset_is_clamped_to_total_input() {
        let usage = normalize_token_usage(
            &serde_json::json!({
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 250}
            }),
            ProviderUsageSemantics::CachedSubsetOfInput,
        );

        assert_eq!(quantity_for(&usage, "prompt_tokens"), None);
        assert_eq!(quantity_for(&usage, "cached_input_tokens"), Some(Decimal::from(100)));
        assert_eq!(usage.context_input_tokens, Some(Decimal::from(100)));
        assert!(usage.cached_input_was_clamped);
    }

    #[test]
    fn typed_policy_and_formal_payload_shape_select_usage_semantics() {
        assert_eq!(
            configured_provider_usage_semantics(ProviderUsagePolicy::DisjointCacheCounters),
            ProviderUsageSemantics::DisjointCacheCounters
        );
        assert_eq!(
            configured_provider_usage_semantics(ProviderUsagePolicy::CachedSubsetOfInput),
            ProviderUsageSemantics::CachedSubsetOfInput
        );
        assert_eq!(
            provider_usage_semantics_for_payload(
                &serde_json::json!({"prompt_tokens_details": {"cached_tokens": 1}}),
                ProviderUsageSemantics::AutoDetect,
            ),
            ProviderUsageSemantics::CachedSubsetOfInput
        );
        assert_eq!(
            provider_usage_semantics_for_payload(
                &serde_json::json!({"cache_read_input_tokens": 1}),
                ProviderUsageSemantics::AutoDetect,
            ),
            ProviderUsageSemantics::DisjointCacheCounters
        );
    }

    #[test]
    fn provider_identity_does_not_select_usage_math() {
        assert_eq!(
            configured_provider_usage_semantics(ProviderUsagePolicy::AutoDetect),
            ProviderUsageSemantics::AutoDetect
        );
    }

    #[test]
    fn derived_rollup_retries_only_transient_internal_failures() {
        assert!(execution_cost_rollup_error_is_retryable(&ApiError::Internal));
        assert!(!execution_cost_rollup_error_is_retryable(&ApiError::Conflict(
            "multiple currencies".to_string(),
        )));
    }

    #[test]
    fn embedding_billing_call_kinds_resolve_embedding_models() {
        for call_kind in ["embed_chunk", "query_embedding"] {
            assert_eq!(billing_model_capability_kind(call_kind), "embedding");
        }
    }

    #[test]
    fn non_embedding_billing_call_kinds_resolve_chat_models() {
        for call_kind in ["graph_extract", "query_answer", "query_compile", "vision_extract"] {
            assert_eq!(billing_model_capability_kind(call_kind), "chat");
        }
    }
}
