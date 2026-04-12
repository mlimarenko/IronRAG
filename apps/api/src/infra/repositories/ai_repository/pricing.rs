use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiPriceCatalogRow {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub price_variant_key: String,
    pub request_input_tokens_min: Option<i32>,
    pub request_input_tokens_max: Option<i32>,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub catalog_scope: String,
    pub workspace_id: Option<Uuid>,
}

pub async fn list_price_catalog(
    postgres: &PgPool,
    model_catalog_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> Result<Vec<AiPriceCatalogRow>, sqlx::Error> {
    match (model_catalog_id, workspace_id) {
        (Some(model_catalog_id), Some(workspace_id)) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and (
                        (catalog_scope = 'workspace_override' and workspace_id = $2)
                        or (catalog_scope = 'system' and workspace_id is null)
                   )
                 order by catalog_scope asc, effective_from desc, id desc",
            )
            .bind(model_catalog_id)
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (Some(model_catalog_id), None) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .bind(model_catalog_id)
            .fetch_all(postgres)
            .await
        }
        (None, Some(workspace_id)) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where workspace_id = $1 or workspace_id is null
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_price_catalog_by_id(
    postgres: &PgPool,
    price_id: Uuid,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "select
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            price_variant_key,
            request_input_tokens_min,
            request_input_tokens_max,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id
         from ai_price_catalog
         where id = $1",
    )
    .bind(price_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_effective_price_catalog_entry(
    postgres: &PgPool,
    model_catalog_id: Uuid,
    billing_unit: &str,
    workspace_id: Option<Uuid>,
    effective_at: DateTime<Utc>,
    price_variant_key: &str,
    request_input_tokens: Option<i32>,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    let rows = match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and billing_unit = $2::billing_unit
                   and effective_from <= $3
                   and (effective_to is null or effective_to > $3)
                   and (
                        (catalog_scope = 'workspace_override' and workspace_id = $4)
                        or (catalog_scope = 'system' and workspace_id is null)
                   )",
            )
            .bind(model_catalog_id)
            .bind(billing_unit)
            .bind(effective_at)
            .bind(workspace_id)
            .fetch_all(postgres)
            .await?
        }
        None => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    price_variant_key,
                    request_input_tokens_min,
                    request_input_tokens_max,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and billing_unit = $2::billing_unit
                   and effective_from <= $3
                   and (effective_to is null or effective_to > $3)",
            )
            .bind(model_catalog_id)
            .bind(billing_unit)
            .bind(effective_at)
            .fetch_all(postgres)
            .await?
        }
    };

    Ok(select_effective_price_catalog_entry(rows, price_variant_key, request_input_tokens))
}

fn select_effective_price_catalog_entry(
    rows: Vec<AiPriceCatalogRow>,
    price_variant_key: &str,
    request_input_tokens: Option<i32>,
) -> Option<AiPriceCatalogRow> {
    rows.into_iter()
        .filter(|row| {
            if price_variant_key == "default" {
                row.price_variant_key == "default"
            } else {
                row.price_variant_key == price_variant_key || row.price_variant_key == "default"
            }
        })
        .filter(|row| match request_input_tokens {
            Some(tokens) => {
                row.request_input_tokens_min.is_none_or(|min| min <= tokens)
                    && row.request_input_tokens_max.is_none_or(|max| max >= tokens)
            }
            None => {
                row.request_input_tokens_min.is_none() && row.request_input_tokens_max.is_none()
            }
        })
        .max_by(|left, right| {
            effective_price_sort_key(left, price_variant_key)
                .cmp(&effective_price_sort_key(right, price_variant_key))
        })
}

fn effective_price_sort_key(
    row: &AiPriceCatalogRow,
    price_variant_key: &str,
) -> (i32, i32, i32, i32, DateTime<Utc>, Uuid) {
    let catalog_scope_rank = if row.catalog_scope == "workspace_override" { 2 } else { 1 };
    let variant_rank = if row.price_variant_key == price_variant_key {
        2
    } else if row.price_variant_key == "default" {
        1
    } else {
        0
    };
    let tier_rank =
        if row.request_input_tokens_min.is_some() || row.request_input_tokens_max.is_some() {
            1
        } else {
            0
        };
    let tier_floor = row.request_input_tokens_min.unwrap_or(-1);
    (catalog_scope_rank, variant_rank, tier_rank, tier_floor, row.effective_from, row.id)
}

pub async fn create_workspace_price_override(
    postgres: &PgPool,
    workspace_id: Uuid,
    model_catalog_id: Uuid,
    billing_unit: &str,
    unit_price: Decimal,
    currency_code: &str,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
) -> Result<AiPriceCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "insert into ai_price_catalog (
            id,
            model_catalog_id,
            billing_unit,
            price_variant_key,
            request_input_tokens_min,
            request_input_tokens_max,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope,
            workspace_id
        )
        values ($1, $2, $3::billing_unit, 'default', null, null, $4, $5, $6, $7, 'workspace_override', $8)
        returning
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            price_variant_key,
            request_input_tokens_min,
            request_input_tokens_max,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id",
    )
    .bind(Uuid::now_v7())
    .bind(model_catalog_id)
    .bind(billing_unit)
    .bind(unit_price)
    .bind(currency_code)
    .bind(effective_from)
    .bind(effective_to)
    .bind(workspace_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_workspace_price_override(
    postgres: &PgPool,
    price_id: Uuid,
    model_catalog_id: Uuid,
    billing_unit: &str,
    unit_price: Decimal,
    currency_code: &str,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "update ai_price_catalog
         set model_catalog_id = $2,
             billing_unit = $3::billing_unit,
             price_variant_key = 'default',
             request_input_tokens_min = null,
             request_input_tokens_max = null,
             unit_price = $4,
             currency_code = $5,
             effective_from = $6,
             effective_to = $7
         where id = $1
           and catalog_scope = 'workspace_override'
         returning
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            price_variant_key,
            request_input_tokens_min,
            request_input_tokens_max,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id",
    )
    .bind(price_id)
    .bind(model_catalog_id)
    .bind(billing_unit)
    .bind(unit_price)
    .bind(currency_code)
    .bind(effective_from)
    .bind(effective_to)
    .fetch_optional(postgres)
    .await
}
