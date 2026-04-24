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
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
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

#[derive(Debug, Clone, FromRow)]
pub struct BillingExecutionProviderCallDescriptorRow {
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
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
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
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

pub async fn list_provider_call_descriptors_by_execution_ids(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_ids: &[Uuid],
) -> Result<Vec<BillingExecutionProviderCallDescriptorRow>, sqlx::Error> {
    if owning_execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, BillingExecutionProviderCallDescriptorRow>(
        "select
            bpc.owning_execution_id,
            bpc.runtime_execution_id,
            apc.provider_kind,
            amc.model_name
         from billing_provider_call bpc
         join ai_provider_catalog apc on apc.id = bpc.provider_catalog_id
         join ai_model_catalog amc on amc.id = bpc.model_catalog_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = any($2)
         order by bpc.owning_execution_id asc, bpc.started_at desc, bpc.id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_ids)
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
            workspace_id,
            library_id,
            knowledge_document_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at
        )
        values ($1, $2::billing_owning_execution_kind, $3, $4, $5, $6, $7, $8, $9, now())
        on conflict (owning_execution_kind, owning_execution_id)
        do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            knowledge_document_id = excluded.knowledge_document_id,
            total_cost = excluded.total_cost,
            currency_code = excluded.currency_code,
            provider_call_count = excluded.provider_call_count,
            updated_at = now()
        returning
            id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            knowledge_document_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.owning_execution_kind)
    .bind(input.owning_execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.knowledge_document_id)
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
            workspace_id,
            library_id,
            knowledge_document_id,
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

pub async fn list_execution_costs_by_execution_ids(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_ids: &[Uuid],
) -> Result<Vec<BillingExecutionCostRow>, sqlx::Error> {
    if owning_execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, BillingExecutionCostRow>(
        "select
            id,
            owning_execution_kind::text as owning_execution_kind,
            owning_execution_id,
            workspace_id,
            library_id,
            knowledge_document_id,
            total_cost,
            currency_code,
            provider_call_count,
            updated_at
         from billing_execution_cost
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = any($2)
         order by updated_at desc, id desc",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_ids)
    .fetch_all(postgres)
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
    // Canonical shape: billing_execution_cost carries library_id +
    // knowledge_document_id directly (migration 0006), so per-document
    // rollup is a single indexed aggregate without the old 5-way LEFT
    // JOIN through provider_call / ingest_attempt / ingest_job /
    // runtime_graph_extraction. The old CTE also re-fanned rows through
    // billing_usage + billing_charge, which produced correct totals
    // only by accident (SUM over a LEFT JOIN that happened to have
    // zero-or-one charge per provider_call row).
    sqlx::query_as::<_, DocumentCostRow>(
        "select
            d.id as document_id,
            coalesce(sum(bec.total_cost), 0) as total_cost,
            coalesce(max(bec.currency_code), 'USD') as currency_code,
            coalesce(sum(bec.provider_call_count), 0)::bigint as provider_call_count
         from content_document d
         left join billing_execution_cost bec
           on bec.library_id = d.library_id
          and bec.knowledge_document_id = d.id
         where d.library_id = $1
           and d.deleted_at is null
         group by d.id, d.created_at
         order by coalesce(sum(bec.total_cost), 0) desc, d.created_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_library_cost_summary(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<LibraryCostSummaryRow>, sqlx::Error> {
    // Canonical shape: read the rollup table directly by library_id.
    // The previous implementation JOINed billing_execution_cost back to
    // billing_provider_call on (owning_execution_kind, owning_execution_id)
    // — billing_execution_cost is UNIQUE on that pair, but provider_call
    // has many rows per execution, so the join fanned each rollup row
    // by the number of provider_call rows, DOUBLING total_cost whenever
    // an execution had more than one provider call. Aside from being
    // ~50× more expensive, the result was numerically wrong.
    sqlx::query_as::<_, LibraryCostSummaryRow>(
        "select
            coalesce(sum(bec.total_cost), 0) as total_cost,
            coalesce(max(bec.currency_code), 'USD') as currency_code,
            count(distinct bec.knowledge_document_id)
                filter (where bec.knowledge_document_id is not null)::bigint as document_count,
            coalesce(sum(bec.provider_call_count), 0)::bigint as provider_call_count
         from billing_execution_cost bec
         where bec.library_id = $1",
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
