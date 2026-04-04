use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::agent_runtime::RuntimeTaskKind,
    domains::billing::{
        BillingCharge, BillingExecutionCost, BillingExecutionOwnerKind, BillingProviderCall,
    },
    infra::repositories::{
        self, ai_repository, billing_repository, catalog_repository, ingest_repository,
        query_repository, runtime_repository,
    },
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCostSummary {
    pub document_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCostSummary {
    pub total_cost: Decimal,
    pub currency_code: String,
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

    pub async fn list_execution_provider_calls(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Vec<BillingProviderCall>, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)?;
        let rows = billing_repository::list_provider_calls_by_execution(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        rows.into_iter().map(map_provider_call_row).collect()
    }

    pub async fn list_execution_charges(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Vec<BillingCharge>, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)?;
        let rows = billing_repository::list_charges_by_execution(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_charge_row).collect())
    }

    pub async fn get_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<BillingExecutionCost, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)?;
        let row = billing_repository::get_execution_cost(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        if let Some(row) = row {
            return map_execution_cost_row(row);
        }

        // Some executions are legitimately zero-cost (no billable provider call captured).
        // Expose deterministic zero-cost truth instead of surfacing an ambiguous 404.
        let provider_call_count = billing_repository::count_provider_calls_by_execution(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        if provider_call_count == 0 {
            return Ok(BillingExecutionCost {
                id: Uuid::now_v7(),
                owning_execution_kind: execution_kind,
                owning_execution_id: execution_id,
                total_cost: Decimal::ZERO,
                currency_code: "USD".to_string(),
                provider_call_count: 0,
                updated_at: Utc::now(),
            });
        }

        Err(ApiError::resource_not_found("billing_execution_cost", execution_id))
    }

    pub async fn list_document_costs_for_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<DocumentCostSummary>, ApiError> {
        let rows = billing_repository::list_document_costs_by_library(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows
            .into_iter()
            .map(|r| DocumentCostSummary {
                document_id: r.document_id,
                total_cost: r.total_cost,
                currency_code: r.currency_code,
                provider_call_count: r.provider_call_count,
            })
            .collect())
    }

    pub async fn get_library_cost_summary(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<LibraryCostSummary, ApiError> {
        let row =
            billing_repository::get_library_cost_summary(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        match row {
            Some(r) => Ok(LibraryCostSummary {
                total_cost: r.total_cost,
                currency_code: r.currency_code,
                document_count: r.document_count,
                provider_call_count: r.provider_call_count,
            }),
            None => Ok(LibraryCostSummary {
                total_cost: Decimal::ZERO,
                currency_code: "USD".to_string(),
                document_count: 0,
                provider_call_count: 0,
            }),
        }
    }

    pub async fn resolve_execution_library_id(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let scope = self
            .resolve_execution_scope(
                state,
                parse_execution_owner_kind(execution_kind)?,
                execution_id,
            )
            .await?;
        Ok(scope.library_id)
    }

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
                call_kind: "query_answer".to_string(),
                usage_json: command.usage_json,
            },
        )
        .await
    }

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

    pub async fn capture_execution_provider_call(
        &self,
        state: &AppState,
        command: CaptureExecutionBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let owning_execution_kind = parse_execution_owner_kind(&command.owning_execution_kind)?;
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
        .map_err(|_| ApiError::Internal)?
        else {
            return Ok(None);
        };
        let Some(model_catalog) = ai_repository::get_model_catalog_by_provider_and_name(
            &state.persistence.postgres,
            &command.provider_kind,
            &command.model_name,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        else {
            return Ok(None);
        };

        let provider_call = billing_repository::create_provider_call(
            &state.persistence.postgres,
            &billing_repository::NewBillingProviderCall {
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
        .map_err(|_| ApiError::Internal)?;

        let request_input_tokens =
            parse_usage_quantity(&command.usage_json, &["prompt_tokens", "input_tokens"])
                .and_then(decimal_to_i32);
        let price_variant_key = extract_price_variant_key(&command.usage_json);
        let usages = extract_token_usage_rows(provider_call.id, &command.usage_json);
        for usage in usages {
            let usage_row = billing_repository::create_usage(&state.persistence.postgres, &usage)
                .await
                .map_err(|_| ApiError::Internal)?;
            let Some(price) = ai_repository::get_effective_price_catalog_entry(
                &state.persistence.postgres,
                model_catalog.id,
                &usage_row.billing_unit,
                Some(command.workspace_id),
                usage_row.observed_at,
                &price_variant_key,
                request_input_tokens,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            else {
                continue;
            };

            let total_price = price.unit_price * usage_row.quantity / Decimal::from(1_000_000u64);
            let _ = billing_repository::create_charge(
                &state.persistence.postgres,
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
            .map_err(|_| ApiError::Internal)?;
        }

        self.roll_up_execution_cost(
            state,
            execution_owner_kind_key(owning_execution_kind),
            command.owning_execution_id,
        )
        .await
    }

    pub async fn roll_up_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)?;
        let provider_call_count = billing_repository::count_provider_calls_by_execution(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let rollups = billing_repository::list_execution_cost_rollups(
            &state.persistence.postgres,
            execution_owner_kind_key(execution_kind),
            execution_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        if rollups.is_empty() {
            return Ok(None);
        }
        if rollups.len() > 1 {
            return Err(ApiError::Conflict(format!(
                "execution {}:{execution_id} has charges in multiple currencies",
                execution_owner_kind_key(execution_kind)
            )));
        }

        let rollup = &rollups[0];
        let provider_call_count = i32::try_from(provider_call_count).unwrap_or(i32::MAX);
        let row = billing_repository::upsert_execution_cost(
            &state.persistence.postgres,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: execution_owner_kind_key(execution_kind),
                owning_execution_id: execution_id,
                total_cost: rollup.total_cost,
                currency_code: &rollup.currency_code,
                provider_call_count,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(Some(map_execution_cost_row(row)?))
    }
}

#[derive(Debug, Clone, Copy)]
struct BillingExecutionScope {
    workspace_id: Uuid,
    library_id: Uuid,
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
                    .map_err(|_| ApiError::Internal)?
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
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
                Ok(BillingExecutionScope {
                    workspace_id: execution.workspace_id,
                    library_id: execution.library_id,
                })
            }
            BillingExecutionOwnerKind::GraphExtractionAttempt => {
                let extraction = repositories::get_runtime_graph_extraction_record_by_id(
                    &state.persistence.postgres,
                    execution_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| {
                    ApiError::resource_not_found("runtime_graph_extraction", execution_id)
                })?;
                let library = catalog_repository::get_library_by_id(
                    &state.persistence.postgres,
                    extraction.project_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("library", extraction.project_id))?;
                Ok(BillingExecutionScope {
                    workspace_id: library.workspace_id,
                    library_id: extraction.project_id,
                })
            }
            BillingExecutionOwnerKind::IngestAttempt => {
                let attempt = ingest_repository::get_ingest_attempt_by_id(
                    &state.persistence.postgres,
                    execution_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", execution_id))?;
                let job = ingest_repository::get_ingest_job_by_id(
                    &state.persistence.postgres,
                    attempt.job_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("ingest_job", attempt.job_id))?;
                Ok(BillingExecutionScope {
                    workspace_id: job.workspace_id,
                    library_id: job.library_id,
                })
            }
        }
    }
}

fn parse_execution_owner_kind(value: &str) -> Result<BillingExecutionOwnerKind, ApiError> {
    match value {
        "query_execution" => Ok(BillingExecutionOwnerKind::QueryExecution),
        "graph_extraction_attempt" => Ok(BillingExecutionOwnerKind::GraphExtractionAttempt),
        "ingest_attempt" => Ok(BillingExecutionOwnerKind::IngestAttempt),
        other => Err(ApiError::BadRequest(format!("unsupported executionKind '{other}'"))),
    }
}

fn extract_token_usage_rows(
    provider_call_id: Uuid,
    usage_json: &Value,
) -> Vec<billing_repository::NewBillingUsage<'static>> {
    let mut rows = Vec::new();
    if let Some(quantity) = parse_usage_quantity(usage_json, &["prompt_tokens", "input_tokens"]) {
        rows.push(billing_repository::NewBillingUsage {
            provider_call_id,
            usage_kind: "prompt_tokens",
            billing_unit: "per_1m_input_tokens",
            quantity,
            observed_at: Some(Utc::now()),
        });
    }
    if let Some(quantity) =
        parse_usage_quantity(usage_json, &["completion_tokens", "output_tokens"])
    {
        rows.push(billing_repository::NewBillingUsage {
            provider_call_id,
            usage_kind: "completion_tokens",
            billing_unit: "per_1m_output_tokens",
            quantity,
            observed_at: Some(Utc::now()),
        });
    }
    if let Some(quantity) = parse_cached_input_quantity(usage_json) {
        rows.push(billing_repository::NewBillingUsage {
            provider_call_id,
            usage_kind: "cached_input_tokens",
            billing_unit: "per_1m_cached_input_tokens",
            quantity,
            observed_at: Some(Utc::now()),
        });
    }
    rows
}

fn parse_usage_quantity(usage_json: &Value, keys: &[&str]) -> Option<Decimal> {
    keys.iter()
        .find_map(|key| usage_json.get(*key))
        .and_then(|value| match value {
            Value::Number(number) => {
                number.as_i64().map(Decimal::from).or_else(|| number.as_u64().map(Decimal::from))
            }
            Value::String(text) => text.parse::<i64>().ok().map(Decimal::from),
            _ => None,
        })
        .filter(|value| *value > Decimal::ZERO)
}

fn parse_cached_input_quantity(usage_json: &Value) -> Option<Decimal> {
    parse_usage_quantity(
        usage_json,
        &["cached_input_tokens", "cache_read_input_tokens", "input_cached_tokens"],
    )
    .or_else(|| {
        usage_json
            .get("prompt_tokens_details")
            .and_then(|details| parse_usage_quantity(details, &["cached_tokens"]))
    })
    .or_else(|| {
        usage_json
            .get("input_tokens_details")
            .and_then(|details| parse_usage_quantity(details, &["cached_tokens"]))
    })
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

fn map_provider_call_row(
    row: billing_repository::BillingProviderCallRow,
) -> Result<BillingProviderCall, ApiError> {
    Ok(BillingProviderCall {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        binding_id: row.binding_id,
        owning_execution_kind: parse_execution_owner_kind(&row.owning_execution_kind)?,
        owning_execution_id: row.owning_execution_id,
        runtime_execution_id: row.runtime_execution_id,
        runtime_task_kind: row
            .runtime_task_kind
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(ApiError::BadRequest)?,
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
) -> Result<BillingExecutionCost, ApiError> {
    Ok(BillingExecutionCost {
        id: row.id,
        owning_execution_kind: parse_execution_owner_kind(&row.owning_execution_kind)?,
        owning_execution_id: row.owning_execution_id,
        total_cost: row.total_cost,
        currency_code: row.currency_code,
        provider_call_count: row.provider_call_count,
        updated_at: row.updated_at,
    })
}

fn execution_owner_kind_key(value: BillingExecutionOwnerKind) -> &'static str {
    match value {
        BillingExecutionOwnerKind::QueryExecution => "query_execution",
        BillingExecutionOwnerKind::GraphExtractionAttempt => "graph_extraction_attempt",
        BillingExecutionOwnerKind::IngestAttempt => "ingest_attempt",
    }
}
