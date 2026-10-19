use crate::{
    domains::graph_quality::ExtractionRecoverySummary,
    services::graph::extract::types::{
        GraphExtractionLifecycle, GraphExtractionRecoveryTrace, GraphExtractionUsageCall,
    },
};

/// The pieces of one terminal graph extraction that are written beside the raw
/// provider output. Grouping them keeps [`build_raw_output_json`] the single
/// owner of the persisted raw-output shape.
pub(crate) struct GraphExtractionRawOutput<'a> {
    pub output_text: &'a str,
    pub usage_json: serde_json::Value,
    pub lifecycle: &'a GraphExtractionLifecycle,
    pub recovery: &'a GraphExtractionRecoveryTrace,
    pub recovery_summary: &'a ExtractionRecoverySummary,
    pub usage_calls: &'a [GraphExtractionUsageCall],
    pub extraction_contract_hash: &'a str,
}

pub(crate) fn build_raw_output_json(record: GraphExtractionRawOutput<'_>) -> serde_json::Value {
    // `prompt_hash` (the full rendered prompt hash) stays on the row column for
    // audit; `extraction_contract_hash` is the stable semantic cache identity
    // (IRG-197) and lives beside the raw output so no schema change is needed.
    serde_json::json!({
        "output_text": record.output_text,
        "usage": record.usage_json,
        "provider_calls": record.usage_calls,
        "lifecycle": record.lifecycle,
        "recovery": record.recovery,
        "recovery_summary": record.recovery_summary,
        "extraction_contract_hash": record.extraction_contract_hash,
    })
}

pub(crate) fn build_provider_usage_json(
    provider_kind: &str,
    model_name: &str,
    usage_json: serde_json::Value,
) -> serde_json::Value {
    let mut payload = usage_json;
    match payload.as_object_mut() {
        Some(object) => {
            object
                .entry("provider_kind".to_string())
                .or_insert_with(|| serde_json::Value::String(provider_kind.to_string()));
            object
                .entry("model_name".to_string())
                .or_insert_with(|| serde_json::Value::String(model_name.to_string()));
            payload
        }
        None => serde_json::json!({
            "provider_kind": provider_kind,
            "model_name": model_name,
            "value": payload,
        }),
    }
}

pub(crate) fn aggregate_provider_usage_json(
    provider_kind: &str,
    model_name: &str,
    usage_samples: &[serde_json::Value],
) -> serde_json::Value {
    let prompt_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("prompt_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let completion_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("completion_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let explicit_total_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("total_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let saw_prompt_tokens = usage_samples
        .iter()
        .any(|value| value.get("prompt_tokens").and_then(serde_json::Value::as_i64).is_some());
    let saw_completion_tokens = usage_samples
        .iter()
        .any(|value| value.get("completion_tokens").and_then(serde_json::Value::as_i64).is_some());
    let saw_total_tokens = usage_samples
        .iter()
        .any(|value| value.get("total_tokens").and_then(serde_json::Value::as_i64).is_some());

    let mut payload = serde_json::json!({
        "aggregation": "sum",
        "provider_kind": provider_kind,
        "model_name": model_name,
        "call_count": usage_samples.len(),
        "prompt_tokens": saw_prompt_tokens.then_some(prompt_tokens),
        "completion_tokens": saw_completion_tokens.then_some(completion_tokens),
        "total_tokens": if saw_total_tokens {
            Some(explicit_total_tokens)
        } else if saw_prompt_tokens || saw_completion_tokens {
            Some(prompt_tokens.saturating_add(completion_tokens))
        } else {
            None
        },
    });
    if let Some(service_tier) = unanimous_service_tier(usage_samples)
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("service_tier".to_string(), serde_json::Value::String(service_tier));
    }
    payload
}

/// The service tier every sampled provider call was served on, or `None` when
/// the samples disagree or none reported one. Billing prices the aggregate as
/// one call, so the actual tier survives only when it is uniform; a mixed set
/// falls back to the standard rate instead of a tier that was true for some of
/// the calls.
fn unanimous_service_tier(usage_samples: &[serde_json::Value]) -> Option<String> {
    let mut tiers = usage_samples
        .iter()
        .filter_map(|value| value.get("service_tier").and_then(serde_json::Value::as_str));
    let first = tiers.next()?.to_string();
    tiers.all(|tier| tier == first).then_some(first)
}
