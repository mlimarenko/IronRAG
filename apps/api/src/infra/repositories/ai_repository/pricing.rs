use rust_decimal::Decimal;
use sqlx::{Executor, FromRow, Postgres};
use uuid::Uuid;

use crate::domains::ai::{ModelLongContextPricing, ModelPricing};

/// Minimal typed catalog policy needed to interpret usage counters before
/// pricing them. Provider and model identities are deliberately excluded:
/// token accounting semantics belong to protocol policy, never identifiers.
#[derive(Debug, Clone, FromRow)]
pub struct ProviderBillingUsageProfileRow {
    pub usage_policy_json: Option<serde_json::Value>,
    pub service_tier_multipliers_json: Option<serde_json::Value>,
}

pub async fn get_provider_billing_usage_profile<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_catalog_id: Uuid,
) -> Result<Option<ProviderBillingUsageProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ProviderBillingUsageProfileRow>(
        "select
            capability_flags_json -> 'usagePolicy' as usage_policy_json,
            capability_flags_json -> 'serviceTierMultipliers' as service_tier_multipliers_json
         from ai_provider_catalog
         where id = $1",
    )
    .bind(provider_catalog_id)
    .fetch_optional(executor)
    .await
}

#[derive(Debug, Clone, FromRow)]
struct ModelPricingRow {
    currency_code: String,
    input_price_per_1m: Decimal,
    cached_input_price_per_1m: Decimal,
    cache_write_input_price_per_1m: Decimal,
    output_price_per_1m: Decimal,
    long_context_threshold_tokens: Option<i32>,
    long_context_input_price_per_1m: Option<Decimal>,
    long_context_cached_input_price_per_1m: Option<Decimal>,
    long_context_output_price_per_1m: Option<Decimal>,
}

/// Vendor rates of one model row, typed for charge computation.
pub async fn get_model_pricing<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    model_catalog_id: Uuid,
) -> Result<Option<ModelPricing>, sqlx::Error> {
    let row = sqlx::query_as::<_, ModelPricingRow>(
        "select
            currency_code,
            input_price_per_1m,
            cached_input_price_per_1m,
            cache_write_input_price_per_1m,
            output_price_per_1m,
            long_context_threshold_tokens,
            long_context_input_price_per_1m,
            long_context_cached_input_price_per_1m,
            long_context_output_price_per_1m
         from ai_model_catalog
         where id = $1",
    )
    .bind(model_catalog_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|row| {
        let long_context = match (
            row.long_context_threshold_tokens,
            row.long_context_input_price_per_1m,
            row.long_context_cached_input_price_per_1m,
            row.long_context_output_price_per_1m,
        ) {
            (Some(threshold_tokens), Some(input), Some(cached_input), Some(output)) => {
                Some(ModelLongContextPricing {
                    threshold_tokens,
                    input_price_per_1m: input,
                    cached_input_price_per_1m: cached_input,
                    output_price_per_1m: output,
                })
            }
            _ => None,
        };
        ModelPricing {
            currency_code: row.currency_code,
            input_price_per_1m: row.input_price_per_1m,
            cached_input_price_per_1m: row.cached_input_price_per_1m,
            cache_write_input_price_per_1m: row.cache_write_input_price_per_1m,
            output_price_per_1m: row.output_price_per_1m,
            long_context,
        }
    }))
}
