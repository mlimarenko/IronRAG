use std::collections::BTreeMap;

use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ProviderUsageSemantics {
    AutoDetect,
    CachedSubsetOfInput,
    DisjointCacheCounters,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct NormalizedTokenUsage {
    pub(super) ordinary_input_tokens: Option<Decimal>,
    pub(super) output_tokens: Option<Decimal>,
    pub(super) cached_input_tokens: Option<Decimal>,
    pub(super) cache_creation_input_tokens: Option<Decimal>,
    /// Total request input used only to select a context-size price tier.
    pub(super) context_input_tokens: Option<Decimal>,
    pub(super) cached_input_was_clamped: bool,
}

impl NormalizedTokenUsage {
    pub(super) fn rows(
        &self,
        provider_call_id: Uuid,
    ) -> Vec<billing_repository::NewBillingUsage<'static>> {
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

pub(super) fn parse_provider_usage_policy(
    value: Option<&Value>,
) -> Result<ProviderUsagePolicy, serde_json::Error> {
    match value {
        Some(value) => serde_json::from_value(value.clone()),
        None => Ok(ProviderUsagePolicy::default()),
    }
}

/// Typed price multipliers the provider profile declares per actual
/// `service_tier`. Absent means the provider publishes no tiered pricing, and
/// every call prices at 1.
pub(super) fn parse_service_tier_multipliers(
    value: Option<&Value>,
) -> Result<BTreeMap<String, Decimal>, serde_json::Error> {
    match value {
        Some(value) => serde_json::from_value(value.clone()),
        None => Ok(BTreeMap::new()),
    }
}

/// Multiplier the catalog declares for the service tier the provider actually
/// served, read from the call's usage payload. An absent, unknown or standard
/// tier prices at 1 — the full rate.
pub(super) fn service_tier_multiplier(
    usage_json: &Value,
    multipliers: &BTreeMap<String, Decimal>,
) -> Decimal {
    usage_json
        .get("service_tier")
        .and_then(Value::as_str)
        .and_then(|tier| multipliers.get(tier))
        .copied()
        .unwrap_or(Decimal::ONE)
}

pub(super) const fn configured_provider_usage_semantics(
    policy: ProviderUsagePolicy,
) -> ProviderUsageSemantics {
    match policy {
        ProviderUsagePolicy::AutoDetect => ProviderUsageSemantics::AutoDetect,
        ProviderUsagePolicy::CachedSubsetOfInput => ProviderUsageSemantics::CachedSubsetOfInput,
        ProviderUsagePolicy::DisjointCacheCounters => ProviderUsageSemantics::DisjointCacheCounters,
    }
}

pub(super) fn provider_usage_semantics_for_payload(
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

pub(super) fn normalize_token_usage(
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

pub(super) fn parse_usage_quantity(usage_json: &Value, keys: &[&str]) -> Option<Decimal> {
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

pub(super) fn parse_subset_cached_input_quantity(usage_json: &Value) -> Option<Decimal> {
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

pub(super) fn positive_quantity(value: Decimal) -> Option<Decimal> {
    (value > Decimal::ZERO).then_some(value)
}

pub(super) fn sum_positive_quantities<const N: usize>(
    values: [Option<Decimal>; N],
) -> Option<Decimal> {
    positive_quantity(values.into_iter().flatten().sum())
}

pub(super) fn decimal_to_i32(value: Decimal) -> Option<i32> {
    value.round().to_i32()
}
