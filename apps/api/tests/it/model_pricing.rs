use ironrag_backend::domains::{
    ai::{ModelLongContextPricing, ModelPricing},
    billing::PricingBillingUnit,
};
use rust_decimal::Decimal;

const LONG_CONTEXT_THRESHOLD_TOKENS: i32 = 100;

fn long_context_pricing() -> ModelPricing {
    ModelPricing {
        currency_code: "USD".to_string(),
        input_price_per_1m: Decimal::from(1),
        cached_input_price_per_1m: Decimal::from(2),
        cache_write_input_price_per_1m: Decimal::from(3),
        output_price_per_1m: Decimal::from(4),
        long_context: Some(ModelLongContextPricing {
            threshold_tokens: LONG_CONTEXT_THRESHOLD_TOKENS,
            input_price_per_1m: Decimal::from(11),
            cached_input_price_per_1m: Decimal::from(12),
            output_price_per_1m: Decimal::from(14),
        }),
    }
}

#[test]
fn model_pricing_uses_long_context_rates_only_after_the_threshold() {
    let pricing = long_context_pricing();

    for (request_input_tokens, expected_input, expected_cached_input, expected_output) in [
        (LONG_CONTEXT_THRESHOLD_TOKENS - 1, Decimal::from(1), Decimal::from(2), Decimal::from(4)),
        (LONG_CONTEXT_THRESHOLD_TOKENS, Decimal::from(1), Decimal::from(2), Decimal::from(4)),
        (
            LONG_CONTEXT_THRESHOLD_TOKENS + 1,
            Decimal::from(11),
            Decimal::from(12),
            Decimal::from(14),
        ),
    ] {
        assert_eq!(
            pricing.unit_price(
                PricingBillingUnit::Per1MInputTokens.as_str(),
                Some(request_input_tokens),
            ),
            expected_input,
            "input rate at {request_input_tokens} request tokens",
        );
        assert_eq!(
            pricing.unit_price(
                PricingBillingUnit::Per1MCachedInputTokens.as_str(),
                Some(request_input_tokens),
            ),
            expected_cached_input,
            "cached-input rate at {request_input_tokens} request tokens",
        );
        assert_eq!(
            pricing.unit_price(
                PricingBillingUnit::Per1MOutputTokens.as_str(),
                Some(request_input_tokens),
            ),
            expected_output,
            "output rate at {request_input_tokens} request tokens",
        );
    }
}
