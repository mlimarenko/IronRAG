use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
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
    pub price_catalog_id: Uuid,
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
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionCostRollupRow {
    pub currency_code: String,
    pub total_cost: Decimal,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone)]
pub struct NewBillingProviderCall<'a> {
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
    pub price_catalog_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpsertBillingExecutionCost<'a> {
    pub owning_execution_kind: &'a str,
    pub owning_execution_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: &'a str,
    pub provider_call_count: i32,
}

pub async fn create_provider_call(
    postgres: &PgPool,
    input: &NewBillingProviderCall<'_>,
) -> Result<BillingProviderCallRow, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "insert into billing_provider_call (
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            started_at,
            completed_at,
            call_state
        )
        values ($1, $2, $3, $4, $5::billing_owning_execution_kind, $6, $7, $8::runtime_task_kind, $9, $10, $11, now(), $12, $13::billing_call_state)
        returning
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.binding_id)
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.runtime_execution_id)
    .bind(input.runtime_task_kind)
    .bind(input.provider_catalog_id)
    .bind(input.model_catalog_id)
    .bind(input.call_kind)
    .bind(input.completed_at)
    .bind(input.call_state)
    .fetch_one(postgres)
    .await
}

pub async fn update_provider_call_state(
    postgres: &PgPool,
    provider_call_id: Uuid,
    call_state: &str,
    completed_at: Option<DateTime<Utc>>,
) -> Result<Option<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "update billing_provider_call
         set call_state = $2::billing_call_state,
             completed_at = $3
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at",
    )
    .bind(provider_call_id)
    .bind(call_state)
    .bind(completed_at)
    .fetch_optional(postgres)
    .await
}

pub async fn list_provider_calls_by_execution(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Vec<BillingProviderCallRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingProviderCallRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            runtime_execution_id,
            runtime_task_kind::text as runtime_task_kind,
            provider_catalog_id,
            model_catalog_id,
            call_kind,
            call_state::text as call_state,
            started_at,
            completed_at
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2
         order by started_at desc, id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_usage(
    postgres: &PgPool,
    input: &NewBillingUsage<'_>,
) -> Result<BillingUsageRow, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "insert into billing_usage (
            id,
            provider_call_id,
            usage_kind,
            billing_unit,
            quantity,
            observed_at
        )
        values ($1, $2, $3, $4::billing_unit, $5, coalesce($6, now()))
        returning
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.provider_call_id)
    .bind(input.usage_kind)
    .bind(input.billing_unit)
    .bind(input.quantity)
    .bind(input.observed_at)
    .fetch_one(postgres)
    .await
}

pub async fn list_usage_by_provider_call(
    postgres: &PgPool,
    provider_call_id: Uuid,
) -> Result<Vec<BillingUsageRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "select
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at
         from billing_usage
         where provider_call_id = $1
         order by observed_at asc, id asc",
    )
    .bind(provider_call_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_charge(
    postgres: &PgPool,
    input: &NewBillingCharge,
) -> Result<BillingChargeRow, sqlx::Error> {
    sqlx::query_as::<_, BillingChargeRow>(
        "insert into billing_charge (
            id,
            usage_id,
            price_catalog_id,
            currency_code,
            unit_price,
            total_price,
            priced_at
        )
        values ($1, $2, $3, $4, $5, $6, coalesce($7, now()))
        returning
            id,
            usage_id,
            price_catalog_id,
            currency_code,
            unit_price,
            total_price,
            priced_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.usage_id)
    .bind(input.price_catalog_id)
    .bind(&input.currency_code)
    .bind(input.unit_price)
    .bind(input.total_price)
    .bind(input.priced_at)
    .fetch_one(postgres)
    .await
}

pub async fn list_charges_by_execution(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Vec<BillingChargeRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingChargeRow>(
        "select
            bc.id,
            bc.usage_id,
            bc.price_catalog_id,
            bc.currency_code,
            bc.unit_price,
            bc.total_price,
            bc.priced_at
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2
         order by bc.priced_at desc, bc.id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_all(postgres)
    .await
}

pub async fn upsert_execution_cost(
    postgres: &PgPool,
    input: &UpsertBillingExecutionCost<'_>,
) -> Result<BillingExecutionCostRow, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRow>(
        "insert into billing_execution_cost (
            id,
            owning_execution_kind,
            owning_execution_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at
        )
        values ($1, $2::billing_owning_execution_kind, $3, $4, $5, $6, now())
        on conflict (owning_execution_kind, owning_execution_id)
        do update set
            total_cost = excluded.total_cost,
            currency_code = excluded.currency_code,
            provider_call_count = excluded.provider_call_count,
            updated_at = now()
        returning
            id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.total_cost)
    .bind(input.currency_code)
    .bind(input.provider_call_count)
    .fetch_one(postgres)
    .await
}

pub async fn get_execution_cost(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Option<BillingExecutionCostRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRow>(
        "select
            id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at
         from billing_execution_cost
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_execution_cost_rollups(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Vec<BillingExecutionCostRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingExecutionCostRollupRow>(
        "select
            bc.currency_code,
            sum(bc.total_price) as total_cost,
            count(distinct bpc.id)::bigint as provider_call_count
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2
         group by bc.currency_code
         order by bc.currency_code asc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Clone, FromRow)]
pub struct DocumentCostRow {
    pub document_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct LibraryCostSummaryRow {
    pub total_cost: Decimal,
    pub currency_code: String,
    pub document_count: i64,
    pub provider_call_count: i64,
}

pub async fn list_document_costs_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<DocumentCostRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentCostRow>(
        "select
            ij.knowledge_document_id as document_id,
            sum(bec.total_cost) as total_cost,
            bec.currency_code,
            sum(bec.provider_call_count)::bigint as provider_call_count
         from billing_execution_cost bec
         join ingest_attempt ia on ia.id = bec.owning_execution_id
         join ingest_job ij on ij.id = ia.job_id
         where bec.owning_execution_kind = 'ingest_attempt'
           and ij.library_id = $1
         group by ij.knowledge_document_id, bec.currency_code
         order by total_cost desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_library_cost_summary(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<LibraryCostSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, LibraryCostSummaryRow>(
        "select
            coalesce(sum(bec.total_cost), 0) as total_cost,
            coalesce(max(bec.currency_code), 'USD') as currency_code,
            count(distinct ij.knowledge_document_id)::bigint as document_count,
            coalesce(sum(bec.provider_call_count), 0)::bigint as provider_call_count
         from billing_execution_cost bec
         join ingest_attempt ia on ia.id = bec.owning_execution_id
         join ingest_job ij on ij.id = ia.job_id
         where bec.owning_execution_kind = 'ingest_attempt'
           and ij.library_id = $1",
    )
    .bind(library_id)
    .fetch_optional(postgres)
    .await
}

pub async fn count_provider_calls_by_execution(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(postgres)
    .await
}
