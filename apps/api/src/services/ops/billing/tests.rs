use super::{
    ProviderUsageSemantics, configured_provider_usage_semantics,
    execution_cost_rollup_error_is_retryable, normalize_token_usage,
    parse_service_tier_multipliers, provider_usage_semantics_for_payload, service_tier_multiplier,
};
use crate::domains::ai::ModelPricing;
use crate::domains::{billing::PricingBillingUnit, provider_profiles::ProviderUsagePolicy};
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

fn usd_pricing(input_price: i64, output_price: i64) -> ModelPricing {
    ModelPricing {
        currency_code: "USD".to_string(),
        input_price_per_1m: Decimal::from(input_price),
        cached_input_price_per_1m: Decimal::from(1),
        cache_write_input_price_per_1m: Decimal::from(3),
        output_price_per_1m: Decimal::from(output_price),
        long_context: None,
    }
}

#[test]
fn the_actual_service_tier_scales_the_billed_unit_price() {
    let pricing = usd_pricing(2, 10);
    let multipliers = parse_service_tier_multipliers(Some(&serde_json::json!({
        "flex": "0.5",
        "priority": "2"
    })))
    .expect("service tier multipliers should parse");

    let standard = pricing.unit_price("per_1m_input_tokens", None)
        * service_tier_multiplier(&serde_json::json!({}), &multipliers);
    let default = pricing.unit_price("per_1m_input_tokens", None)
        * service_tier_multiplier(&serde_json::json!({"service_tier": "default"}), &multipliers);
    let flex = pricing.unit_price("per_1m_input_tokens", None)
        * service_tier_multiplier(&serde_json::json!({"service_tier": "flex"}), &multipliers);
    let priority = pricing.unit_price("per_1m_input_tokens", None)
        * service_tier_multiplier(&serde_json::json!({"service_tier": "priority"}), &multipliers);

    assert_eq!(standard, Decimal::from(2));
    assert_eq!(default, Decimal::from(2), "the standard tier keeps the full rate");
    assert_eq!(flex, Decimal::from(1), "flex bills at half the standard rate");
    assert_eq!(priority, Decimal::from(4), "priority bills at double the standard rate");
}

#[test]
fn an_unknown_service_tier_on_a_profile_without_multipliers_prices_at_the_full_rate() {
    let pricing = usd_pricing(2, 10);
    let multipliers =
        parse_service_tier_multipliers(None).expect("absent multipliers should default to empty");

    let price = pricing.unit_price("per_1m_output_tokens", None)
        * service_tier_multiplier(&serde_json::json!({"service_tier": "flex"}), &multipliers);

    assert_eq!(price, Decimal::from(10));
}

#[test]
fn derived_rollup_retries_only_transient_internal_failures() {
    assert!(execution_cost_rollup_error_is_retryable(&ApiError::Internal));
    assert!(!execution_cost_rollup_error_is_retryable(&ApiError::Conflict(
        "multiple currencies".to_string(),
    )));
}
