use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiProviderCatalogRow {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiModelCatalogRow {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub lifecycle_state: String,
    pub metadata_json: Value,
    pub allowed_binding_purposes: Vec<String>,
    pub currency_code: String,
    pub input_price_per_1m: Decimal,
    pub cached_input_price_per_1m: Decimal,
    pub cache_write_input_price_per_1m: Decimal,
    pub output_price_per_1m: Decimal,
    pub long_context_threshold_tokens: Option<i32>,
    pub long_context_input_price_per_1m: Option<Decimal>,
    pub long_context_cached_input_price_per_1m: Option<Decimal>,
    pub long_context_output_price_per_1m: Option<Decimal>,
}

/// Pricing bind bundle for model CRUD; mirrors the pricing columns.
#[derive(Debug, Clone)]
pub struct ModelPricingBinds {
    pub currency_code: String,
    pub input_price_per_1m: Decimal,
    pub cached_input_price_per_1m: Decimal,
    pub cache_write_input_price_per_1m: Decimal,
    pub output_price_per_1m: Decimal,
    pub long_context_threshold_tokens: Option<i32>,
    pub long_context_input_price_per_1m: Option<Decimal>,
    pub long_context_cached_input_price_per_1m: Option<Decimal>,
    pub long_context_output_price_per_1m: Option<Decimal>,
}

type ModelCatalogQuery<'q> =
    sqlx::query::QueryAs<'q, Postgres, AiModelCatalogRow, sqlx::postgres::PgArguments>;

/// Appends the nine pricing binds in column order for the model CRUD CTEs.
fn bind_model_pricing<'q>(
    query: ModelCatalogQuery<'q>,
    pricing: &'q ModelPricingBinds,
) -> ModelCatalogQuery<'q> {
    query
        .bind(&pricing.currency_code)
        .bind(pricing.input_price_per_1m)
        .bind(pricing.cached_input_price_per_1m)
        .bind(pricing.cache_write_input_price_per_1m)
        .bind(pricing.output_price_per_1m)
        .bind(pricing.long_context_threshold_tokens)
        .bind(pricing.long_context_input_price_per_1m)
        .bind(pricing.long_context_cached_input_price_per_1m)
        .bind(pricing.long_context_output_price_per_1m)
}

/// Column list returned by the model-CRUD CTE queries, parameterized by the
/// CTE alias so the create and update paths share one projection.
macro_rules! model_cte_projection {
    ($alias:literal) => {
        concat!(
            "select
            ",
            $alias,
            ".id,
            ",
            $alias,
            ".provider_catalog_id,
            ",
            $alias,
            ".model_name,
            ",
            $alias,
            ".context_window,
            ",
            $alias,
            ".max_output_tokens,
            ",
            $alias,
            ".lifecycle_state::text as lifecycle_state,
            ",
            $alias,
            ".metadata_json,
            ",
            $alias,
            ".currency_code,
            ",
            $alias,
            ".input_price_per_1m,
            ",
            $alias,
            ".cached_input_price_per_1m,
            ",
            $alias,
            ".cache_write_input_price_per_1m,
            ",
            $alias,
            ".output_price_per_1m,
            ",
            $alias,
            ".long_context_threshold_tokens,
            ",
            $alias,
            ".long_context_input_price_per_1m,
            ",
            $alias,
            ".long_context_cached_input_price_per_1m,
            ",
            $alias,
            ".long_context_output_price_per_1m,
            "
        )
    };
}

macro_rules! model_catalog_query {
    ($suffix:literal) => {
        concat!(
            "select
                id,
                provider_catalog_id,
                model_name,
                context_window,
                max_output_tokens,
                lifecycle_state::text as lifecycle_state,
                metadata_json,
                currency_code,
                input_price_per_1m,
                cached_input_price_per_1m,
                cache_write_input_price_per_1m,
                output_price_per_1m,
                long_context_threshold_tokens,
                long_context_input_price_per_1m,
                long_context_cached_input_price_per_1m,
                long_context_output_price_per_1m,
                array(
                    select relation.binding_purpose::text
                    from ai_model_binding_purpose relation
                    where relation.model_catalog_id = ai_model_catalog.id
                    order by relation.binding_purpose
                ) as allowed_binding_purposes
             from ai_model_catalog",
            $suffix,
        )
    };
}

pub async fn list_provider_catalog(
    postgres: &PgPool,
) -> Result<Vec<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "select
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json
         from ai_provider_catalog
         order by provider_kind asc, id asc",
    )
    .fetch_all(postgres)
    .await
}

pub async fn list_model_catalog(
    postgres: &PgPool,
    provider_catalog_id: Option<Uuid>,
) -> Result<Vec<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(model_catalog_query!(
        " where $1::uuid is null or provider_catalog_id = $1
          order by model_name asc, id asc"
    ))
    .bind(provider_catalog_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_provider_catalog_by_id(
    postgres: &PgPool,
    provider_catalog_id: Uuid,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "select
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json
         from ai_provider_catalog
         where id = $1",
    )
    .bind(provider_catalog_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_model_catalog_by_id(
    postgres: &PgPool,
    model_catalog_id: Uuid,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(model_catalog_query!(" where id = $1"))
        .bind(model_catalog_id)
        .fetch_optional(postgres)
        .await
}

pub async fn get_provider_catalog_by_kind(
    postgres: &PgPool,
    provider_kind: &str,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "select
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json
         from ai_provider_catalog
         where provider_kind = $1
           and lifecycle_state = 'active'",
    )
    .bind(provider_kind)
    .fetch_optional(postgres)
    .await
}

/// Inputs for [`create_provider_catalog`].
#[derive(Debug, Clone)]
pub struct NewProviderCatalog<'a> {
    pub provider_kind: &'a str,
    pub display_name: &'a str,
    pub api_style: &'a str,
    pub lifecycle_state: &'a str,
    pub default_base_url: Option<&'a str>,
    pub capability_flags_json: Value,
}

pub async fn create_provider_catalog(
    postgres: &PgPool,
    provider: NewProviderCatalog<'_>,
) -> Result<AiProviderCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "insert into ai_provider_catalog (
            id,
            provider_kind,
            display_name,
            api_style,
            lifecycle_state,
            default_base_url,
            capability_flags_json
         )
         values (
            uuidv7(),
            $1,
            $2,
            $3::ai_provider_api_style,
            $4::ai_provider_lifecycle_state,
            $5,
            $6
         )
         returning
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json",
    )
    .bind(provider.provider_kind)
    .bind(provider.display_name)
    .bind(provider.api_style)
    .bind(provider.lifecycle_state)
    .bind(provider.default_base_url)
    .bind(provider.capability_flags_json)
    .fetch_one(postgres)
    .await
}

/// Inputs for [`update_provider_catalog`].
#[derive(Debug, Clone)]
pub struct UpdateProviderCatalog<'a> {
    pub provider_id: Uuid,
    pub provider_kind: &'a str,
    pub display_name: &'a str,
    pub api_style: &'a str,
    pub lifecycle_state: &'a str,
    pub default_base_url: Option<&'a str>,
    pub capability_flags_json: Option<Value>,
}

pub async fn update_provider_catalog(
    postgres: &PgPool,
    provider: UpdateProviderCatalog<'_>,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "update ai_provider_catalog
         set provider_kind = $2,
             display_name = $3,
             api_style = $4::ai_provider_api_style,
             lifecycle_state = $5::ai_provider_lifecycle_state,
             default_base_url = $6,
             capability_flags_json = coalesce($7::jsonb, capability_flags_json)
         where id = $1
         returning
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json",
    )
    .bind(provider.provider_id)
    .bind(provider.provider_kind)
    .bind(provider.display_name)
    .bind(provider.api_style)
    .bind(provider.lifecycle_state)
    .bind(provider.default_base_url)
    .bind(provider.capability_flags_json)
    .fetch_optional(postgres)
    .await
}

/// Real hard delete. The provider row's `on delete restrict` foreign keys
/// (models, accounts) surface as a foreign-key violation that
/// `map_ai_delete_error` maps to a 409 conflict. The reversible pause is
/// `update_provider_catalog` with `lifecycleState: "disabled"`, a distinct
/// operation from this irreversible delete (see the AI section of
/// `memory/2026-07-17-rest-api-query-refactor-plan.md`).
pub async fn delete_provider_catalog(
    postgres: &PgPool,
    provider_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from ai_provider_catalog where id = $1")
        .bind(provider_id)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_model_catalog_by_provider_and_name(
    postgres: &PgPool,
    provider_kind: &str,
    model_name: &str,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(concat!(
        model_cte_projection!("amc"),
        "array(
                select relation.binding_purpose::text
                from ai_model_binding_purpose relation
                where relation.model_catalog_id = amc.id
                order by relation.binding_purpose
            ) as allowed_binding_purposes
         from ai_model_catalog amc
         join ai_provider_catalog apc on apc.id = amc.provider_catalog_id
         where apc.provider_kind = $1
           and apc.lifecycle_state = 'active'
           and amc.model_name = $2
           and amc.lifecycle_state = 'active'
         order by
            amc.id asc
         limit 1",
    ))
    .bind(provider_kind)
    .bind(model_name)
    .fetch_optional(postgres)
    .await
}

/// Inputs for [`create_model_catalog`].
#[derive(Debug, Clone)]
pub struct NewModelCatalog<'a> {
    pub provider_catalog_id: Uuid,
    pub model_name: &'a str,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub lifecycle_state: &'a str,
    pub metadata_json: Value,
    pub pricing: &'a ModelPricingBinds,
    pub allowed_binding_purposes: &'a [String],
}

pub async fn create_model_catalog(
    postgres: &PgPool,
    model: NewModelCatalog<'_>,
) -> Result<AiModelCatalogRow, sqlx::Error> {
    bind_model_pricing(
        sqlx::query_as::<_, AiModelCatalogRow>(concat!(
            "with inserted_model as (
            insert into ai_model_catalog (
                id,
                provider_catalog_id,
                model_name,
                context_window,
                max_output_tokens,
                lifecycle_state,
                metadata_json,
                currency_code,
                input_price_per_1m,
                cached_input_price_per_1m,
                cache_write_input_price_per_1m,
                output_price_per_1m,
                long_context_threshold_tokens,
                long_context_input_price_per_1m,
                long_context_cached_input_price_per_1m,
                long_context_output_price_per_1m
            )
            values (
                uuidv7(),
                $1,
                $2,
                $3,
                $4,
                $5::ai_model_lifecycle_state,
                $6,
                $8,
                $9,
                $10,
                $11,
                $12,
                $13,
                $14,
                $15,
                $16
            )
            returning *
         ), inserted_purposes as (
            insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
            select inserted_model.id, purpose::ai_binding_purpose
            from inserted_model
            cross join unnest($7::text[]) as purpose
            on conflict (model_catalog_id, binding_purpose) do nothing
            returning binding_purpose
         )
         ",
            model_cte_projection!("inserted_model"),
            "array(
                select inserted_purposes.binding_purpose::text
                from inserted_purposes
                order by inserted_purposes.binding_purpose
            ) as allowed_binding_purposes
         from inserted_model",
        ))
        .bind(model.provider_catalog_id)
        .bind(model.model_name)
        .bind(model.context_window)
        .bind(model.max_output_tokens)
        .bind(model.lifecycle_state)
        .bind(model.metadata_json)
        .bind(model.allowed_binding_purposes),
        model.pricing,
    )
    .fetch_one(postgres)
    .await
}

/// Inputs for [`update_model_catalog`].
#[derive(Debug, Clone)]
pub struct UpdateModelCatalog<'a> {
    pub model_id: Uuid,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub lifecycle_state: &'a str,
    pub metadata_json: Value,
    pub pricing: &'a ModelPricingBinds,
    pub allowed_binding_purposes: &'a [String],
}

pub async fn update_model_catalog(
    postgres: &PgPool,
    model: UpdateModelCatalog<'_>,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    bind_model_pricing(
        sqlx::query_as::<_, AiModelCatalogRow>(concat!(
            "with desired_purposes as (
            select distinct purpose::ai_binding_purpose as binding_purpose
            from unnest($6::text[]) as purpose
         ), updated_model as (
            update ai_model_catalog
            set context_window = $2,
                max_output_tokens = $3,
                lifecycle_state = $4::ai_model_lifecycle_state,
                metadata_json = $5,
                currency_code = $7,
                input_price_per_1m = $8,
                cached_input_price_per_1m = $9,
                cache_write_input_price_per_1m = $10,
                output_price_per_1m = $11,
                long_context_threshold_tokens = $12,
                long_context_input_price_per_1m = $13,
                long_context_cached_input_price_per_1m = $14,
                long_context_output_price_per_1m = $15
            where id = $1
            returning *
         ), inserted_purposes as (
            insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
            select updated_model.id, desired_purposes.binding_purpose
            from updated_model
            cross join desired_purposes
            on conflict (model_catalog_id, binding_purpose) do nothing
         ), deleted_purposes as (
            delete from ai_model_binding_purpose relation
            using updated_model
            where relation.model_catalog_id = updated_model.id
              and not exists (
                  select 1
                  from desired_purposes
                  where desired_purposes.binding_purpose = relation.binding_purpose
              )
         )
         ",
            model_cte_projection!("updated_model"),
            "array(
                select desired_purposes.binding_purpose::text
                from desired_purposes
                order by desired_purposes.binding_purpose
            ) as allowed_binding_purposes
         from updated_model",
        ))
        .bind(model.model_id)
        .bind(model.context_window)
        .bind(model.max_output_tokens)
        .bind(model.lifecycle_state)
        .bind(model.metadata_json)
        .bind(model.allowed_binding_purposes),
        model.pricing,
    )
    .fetch_optional(postgres)
    .await
}

/// Real hard delete. The model row's `on delete restrict` foreign keys
/// (bindings) surface as a
/// foreign-key violation that `map_ai_delete_error` maps to a 409 conflict.
/// The reversible pause is `update_model_catalog` with
/// `lifecycleState: "disabled"`, a distinct operation from this
/// irreversible delete.
pub async fn delete_model_catalog(postgres: &PgPool, model_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from ai_model_catalog where id = $1")
        .bind(model_id)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    fn test_pricing_binds() -> ModelPricingBinds {
        ModelPricingBinds {
            currency_code: "USD".to_string(),
            input_price_per_1m: Decimal::ZERO,
            cached_input_price_per_1m: Decimal::ZERO,
            cache_write_input_price_per_1m: Decimal::ZERO,
            output_price_per_1m: Decimal::ZERO,
            long_context_threshold_tokens: None,
            long_context_input_price_per_1m: None,
            long_context_cached_input_price_per_1m: None,
            long_context_output_price_per_1m: None,
        }
    }

    fn unavailable_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(1))
            .connect_lazy("postgres://postgres@127.0.0.1:1/ironrag")
            .expect("static PostgreSQL URL must parse")
    }

    #[tokio::test]
    async fn catalog_queries_propagate_database_unavailability() {
        let postgres = unavailable_pool();
        let id = Uuid::nil();
        let purposes = vec!["query_answer".to_string()];

        assert!(list_provider_catalog(&postgres).await.is_err());
        assert!(list_model_catalog(&postgres, Some(id)).await.is_err());
        assert!(get_provider_catalog_by_id(&postgres, id).await.is_err());
        assert!(get_provider_catalog_by_kind(&postgres, "openai").await.is_err());
        assert!(get_model_catalog_by_id(&postgres, id).await.is_err());
        assert!(
            get_model_catalog_by_provider_and_name(&postgres, "openai", "model-alpha")
                .await
                .is_err()
        );
        assert!(
            create_provider_catalog(
                &postgres,
                NewProviderCatalog {
                    provider_kind: "provider-alpha",
                    display_name: "Provider Alpha",
                    api_style: "openai_compatible",
                    lifecycle_state: "active",
                    default_base_url: Some("https://provider.invalid/v1"),
                    capability_flags_json: json!({}),
                },
            )
            .await
            .is_err()
        );
        assert!(
            update_provider_catalog(
                &postgres,
                UpdateProviderCatalog {
                    provider_id: id,
                    provider_kind: "provider-alpha",
                    display_name: "Provider Alpha",
                    api_style: "openai_compatible",
                    lifecycle_state: "active",
                    default_base_url: None,
                    capability_flags_json: Some(json!({})),
                },
            )
            .await
            .is_err()
        );
        assert!(delete_provider_catalog(&postgres, id).await.is_err());
        assert!(
            create_model_catalog(
                &postgres,
                NewModelCatalog {
                    provider_catalog_id: id,
                    model_name: "model-alpha",
                    context_window: Some(8_192),
                    max_output_tokens: Some(2_048),
                    lifecycle_state: "active",
                    metadata_json: json!({}),
                    pricing: &test_pricing_binds(),
                    allowed_binding_purposes: &purposes,
                },
            )
            .await
            .is_err()
        );
        assert!(
            update_model_catalog(
                &postgres,
                UpdateModelCatalog {
                    model_id: id,
                    context_window: Some(8_192),
                    max_output_tokens: Some(2_048),
                    lifecycle_state: "active",
                    metadata_json: json!({}),
                    pricing: &test_pricing_binds(),
                    allowed_binding_purposes: &purposes,
                },
            )
            .await
            .is_err()
        );
        assert!(delete_model_catalog(&postgres, id).await.is_err());
    }
}
