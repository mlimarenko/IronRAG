use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct BillingProviderCallRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<String>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: String,
    pub call_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingUsageRow {
    pub id: Uuid,
    pub provider_call_id: Uuid,
    pub usage_kind: String,
    pub billing_unit: String,
    pub quantity: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingChargeRow {
    pub id: Uuid,
    pub usage_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionCostRow {
    pub id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub unpriced_provider_call_count: i32,
    pub updated_at: DateTime<Utc>,
}

macro_rules! billing_provider_call_projection {
    ($prefix:literal) => {
        concat!(
            $prefix,
            "id, ",
            $prefix,
            "workspace_id, ",
            $prefix,
            "library_id, ",
            $prefix,
            "binding_id, ",
            $prefix,
            "owning_execution_kind::text as owning_execution_kind, ",
            $prefix,
            "owning_execution_id, ",
            $prefix,
            "runtime_execution_id, ",
            $prefix,
            "runtime_task_kind::text as runtime_task_kind, ",
            $prefix,
            "provider_catalog_id, ",
            $prefix,
            "model_catalog_id, ",
            $prefix,
            "call_kind, ",
            $prefix,
            "call_state::text as call_state, ",
            $prefix,
            "started_at, ",
            $prefix,
            "completed_at"
        )
    };
}

macro_rules! billing_charge_projection {
    ($prefix:literal) => {
        concat!(
            $prefix,
            "id, ",
            $prefix,
            "usage_id, ",
            $prefix,
            "currency_code, ",
            $prefix,
            "unit_price, ",
            $prefix,
            "total_price, ",
            $prefix,
            "priced_at"
        )
    };
}

macro_rules! billing_execution_cost_projection {
    ($prefix:literal) => {
        concat!(
            $prefix,
            "id, ",
            $prefix,
            "owning_execution_kind::text as owning_execution_kind, ",
            $prefix,
            "owning_execution_id, ",
            $prefix,
            "workspace_id, ",
            $prefix,
            "library_id, ",
            $prefix,
            "knowledge_document_id, ",
            $prefix,
            "total_cost, ",
            $prefix,
            "currency_code, ",
            $prefix,
            "provider_call_count, ",
            $prefix,
            "unpriced_provider_call_count, ",
            $prefix,
            "updated_at"
        )
    };
}

/// One statement-level view of an execution's pipeline billing generation
/// and its derived scalar cost. Keeping these fields in one SQL result prevents
/// a writer from committing between a health check and the aggregate read.
#[derive(Debug, Clone)]
pub struct BillingExecutionCostReadSnapshot {
    pub rollup_state_present: bool,
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub grounded_answer_provider_call_count: i64,
    pub execution_cost: Option<BillingExecutionCostRow>,
}

/// One statement-level view of a scope's rollup health and aggregate rows.
///
/// `rows` is empty while the scope is dirty or terminal, making it impossible
/// for a caller to accidentally expose an older scalar total after ignoring
/// the health fields.
#[derive(Debug, Clone)]
pub struct BillingCostReadSnapshot<T> {
    pub rollup_dirty: bool,
    pub terminal_error_code: Option<String>,
    pub rows: Vec<T>,
}

#[derive(Debug, Clone, FromRow)]
struct BillingExecutionCostReadSnapshotRow {
    rollup_state_present: bool,
    rollup_dirty: bool,
    terminal_error_code: Option<String>,
    grounded_answer_provider_call_count: i64,
    id: Option<Uuid>,
    owning_execution_kind: Option<String>,
    owning_execution_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    knowledge_document_id: Option<Uuid>,
    total_cost: Option<Decimal>,
    currency_code: Option<String>,
    provider_call_count: Option<i32>,
    unpriced_provider_call_count: Option<i32>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionCostRollupRow {
    pub currency_code: String,
    pub total_cost: Decimal,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionProviderCallDescriptorRow {
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
}

/// Durable generation cursor for one derived execution-cost aggregate.
///
/// Pipeline billing writes increment `dirty_generation` in their own
/// transaction. A rollup may advance `applied_generation` only while the
/// observed dirty generation is unchanged, so a concurrent completion can
/// never be acknowledged by an aggregate that did not observe it.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct BillingExecutionCostRollupStateRow {
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub dirty_generation: i64,
    pub applied_generation: i64,
    pub dirty_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
    pub repair_attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub terminal_error_code: Option<String>,
}

/// One unique execution affected by an atomic stale-reservation sweep.
/// `reaped_provider_calls` is bounded by [`MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT`]
/// across the complete result set.
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct ReapedBillingProviderCallExecutionRow {
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub reaped_provider_calls: i64,
}

/// Repository-level safety ceiling. Maintenance callers may request a smaller
/// batch, but can never turn one sweep into an unbounded write or rollup fanout.
pub const MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT: i64 = 100;

/// Repository-level safety ceiling for one durable rollup-repair sweep.
pub const MAX_DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT: i64 = 100;

#[derive(Debug, Clone)]
pub struct NewBillingProviderCall<'a> {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: &'a str,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<&'a str>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: &'a str,
    pub call_state: &'a str,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewBillingUsage<'a> {
    pub provider_call_id: Uuid,
    pub usage_kind: &'a str,
    pub billing_unit: &'a str,
    pub quantity: Decimal,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewBillingCharge {
    pub usage_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpsertBillingExecutionCost<'a> {
    pub owning_execution_kind: &'a str,
    pub owning_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
    pub total_cost: Decimal,
    pub currency_code: &'a str,
    pub provider_call_count: i32,
    pub unpriced_provider_call_count: i32,
}

/// Cost columns every scope aggregate shares.
struct ScopeCostFields {
    total_cost: Decimal,
    document_count: i64,
    provider_call_count: i64,
    unpriced_provider_call_count: i64,
}

fn required_scope_cost_fields(
    total_cost: Option<Decimal>,
    document_count: Option<i64>,
    provider_call_count: Option<i64>,
    unpriced_provider_call_count: Option<i64>,
) -> Result<ScopeCostFields, sqlx::Error> {
    Ok(ScopeCostFields {
        total_cost: required_snapshot_field(total_cost, "total_cost")?,
        document_count: required_snapshot_field(document_count, "document_count")?,
        provider_call_count: required_snapshot_field(provider_call_count, "provider_call_count")?,
        unpriced_provider_call_count: required_snapshot_field(
            unpriced_provider_call_count,
            "unpriced_provider_call_count",
        )?,
    })
}

fn required_snapshot_field<T>(value: Option<T>, field: &str) -> Result<T, sqlx::Error> {
    value.ok_or_else(|| {
        sqlx::Error::Protocol(format!(
            "billing snapshot returned a partial aggregate row: missing {field}"
        ))
    })
}

mod aggregate_costs;
mod execution_costs;
mod provider_calls;
mod usage_charges;

pub use aggregate_costs::*;
pub use execution_costs::*;
pub use provider_calls::*;
pub use usage_charges::*;
