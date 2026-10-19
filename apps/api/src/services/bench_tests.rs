//! Shared support of the live provider benchmarks (entity typing, IRG-184;
//! graph extraction, IRG-207; semantic rerank, IRG-186).
//!
//! Every benchmark builds its bindings from the canonical catalog seed, parses
//! chat arms the same way and prices and times calls the same way, so this
//! module owns that once. It sits at the `services` root because the rerank
//! benchmark lives under `services::query` and the graph ones under
//! `services::graph`; neither is the other's owner. The file name keeps the
//! SonarQube `*_tests.rs` test classification and the module is declared under
//! `#[cfg(test)]`, so none of it is linked into the product.

use uuid::Uuid;

use crate::{
    app::{seeded_model_rate, seeded_provider},
    domains::ai::AiBindingPurpose,
    infra::repositories::{ChunkRow, DocumentRow},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        graph::extract::{
            GraphExtractionRequest, GraphExtractionStructuredChunkContext,
            GraphExtractionSubTypeHints,
        },
    },
};

/// Provider kind of the self-hosted gateway every chat arm goes through.
pub(crate) const LITELLM_KIND: &str = "litellm";

/// One benchmark arm: a resolved binding plus the rate used to price it.
pub(crate) struct Arm {
    pub(crate) name: String,
    pub(crate) binding: ResolvedRuntimeBinding,
    /// USD per one million input and output tokens, when the seed prices it.
    pub(crate) rate: Option<(f64, f64)>,
    pub(crate) rate_source: String,
}

impl Arm {
    #[must_use]
    pub(crate) fn cost_usd(&self, input_tokens: u64, output_tokens: u64) -> Option<f64> {
        self.rate.map(|(input, output)| {
            (input_tokens as f64 * input + output_tokens as f64 * output) / 1_000_000.0
        })
    }
}

#[must_use]
pub(crate) fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// A resolved binding on a seeded provider: its profile, API style and the
/// given endpoint, model and extra parameters.
#[must_use]
pub(crate) fn seeded_binding(
    purpose: AiBindingPurpose,
    provider_kind: &str,
    base_url: &str,
    api_key: String,
    model_name: &str,
    extra_parameters: serde_json::Map<String, serde_json::Value>,
) -> ResolvedRuntimeBinding {
    let seed = seeded_provider(provider_kind).expect("benchmark provider is seeded");
    let mut extra_parameters_json = serde_json::Value::Object(extra_parameters);
    extra_parameters_json["_providerProfile"] = seed.profile;
    ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose: purpose,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: provider_kind.to_string(),
        provider_base_url: Some(base_url.to_string()),
        provider_api_style: seed.api_style.to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some(api_key),
        model_catalog_id: Uuid::now_v7(),
        model_name: model_name.to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json,
    }
}

/// A `GraphExtractionRequest` for one benchmark chunk: ephemeral document and
/// chunk identity and the corpus text, with no product state attached. The
/// graph-extraction benchmark (IRG-207) and the entity-typing benchmark's
/// LLM-only arm (IRG-184) both feed corpus text through the same product path,
/// so the request shape is built once here.
#[must_use]
pub(crate) fn graph_extraction_request(
    source: &str,
    title: &str,
    content: &str,
    ordinal: i32,
) -> GraphExtractionRequest {
    let document_id = Uuid::now_v7();
    let now = chrono::Utc::now();
    GraphExtractionRequest {
        library_id: Uuid::nil(),
        document: DocumentRow {
            id: document_id,
            library_id: Uuid::nil(),
            source_id: None,
            external_key: source.to_string(),
            title: Some(title.to_string()),
            mime_type: Some("text/markdown".to_string()),
            checksum: None,
            active_revision_id: None,
            document_state: "active".to_string(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: None,
            created_at: now,
            updated_at: now,
        },
        chunk: ChunkRow {
            id: Uuid::now_v7(),
            document_id,
            library_id: Uuid::nil(),
            ordinal,
            content: content.to_string(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at: now,
        },
        structured_chunk: GraphExtractionStructuredChunkContext::default(),
        technical_facts: Vec::new(),
        revision_id: None,
        activated_by_attempt_id: None,
        resume_hint: None,
        library_extraction_prompt: None,
        sub_type_hints: GraphExtractionSubTypeHints::default(),
    }
}

/// OpenAI prices its Flex and Batch tiers at half the standard rate
/// (<https://platform.openai.com/docs/pricing>); the catalog seed carries only
/// the standard rate, so a Flex arm is priced with this factor.
const FLEX_RATE_FACTOR: f64 = 0.5;

/// Chat arms on the seeded `litellm` profile from `<prefix>_LITELLM_API_KEY`,
/// `<prefix>_LITELLM_BASE_URL` and `<prefix>_LITELLM_ARMS`; `None` when any of
/// the three is absent. Each comma-separated arm is
/// `<model>[@<reasoning_effort>][~<service_tier>]`, for example
/// `openai/gpt-6-luna@none~flex`.
#[must_use]
pub(crate) fn litellm_arms(prefix: &str, purpose: AiBindingPurpose) -> Option<Vec<Arm>> {
    let api_key = env_value(&format!("{prefix}_LITELLM_API_KEY"))?;
    let base_url = env_value(&format!("{prefix}_LITELLM_BASE_URL"))?;
    let spec = env_value(&format!("{prefix}_LITELLM_ARMS"))?;
    Some(
        spec.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| litellm_arm(entry, purpose, &base_url, &api_key))
            .collect(),
    )
}

fn litellm_arm(entry: &str, purpose: AiBindingPurpose, base_url: &str, api_key: &str) -> Arm {
    let (head, tier) = match entry.split_once('~') {
        Some((head, tier)) => (head, Some(tier)),
        None => (entry, None),
    };
    let (model, effort) = match head.split_once('@') {
        Some((model, effort)) => (model, Some(effort)),
        None => (head, None),
    };
    let mut extra = serde_json::Map::new();
    if let Some(effort) = effort {
        extra.insert("reasoning_effort".to_string(), serde_json::json!(effort));
    }
    let (mut rate, mut rate_source) = gateway_alias_rate(model);
    if let Some(tier) = tier {
        extra.insert("service_tier".to_string(), serde_json::json!(tier));
        if tier == "flex" {
            rate =
                rate.map(|(input, output)| (input * FLEX_RATE_FACTOR, output * FLEX_RATE_FACTOR));
            rate_source = format!("{rate_source}; flex tier at {FLEX_RATE_FACTOR}x standard");
        } else {
            rate = None;
            rate_source = format!("unpriced: service tier {tier} has no seeded rate");
        }
    }
    Arm {
        name: format!("{LITELLM_KIND}/{entry}"),
        binding: seeded_binding(purpose, LITELLM_KIND, base_url, api_key.to_string(), model, extra),
        rate,
        rate_source,
    }
}

fn parse_rate(rate: (&str, &str)) -> Option<(f64, f64)> {
    Some((rate.0.parse().ok()?, rate.1.parse().ok()?))
}

/// A gateway alias `<vendor>/<model>` is priced at the vendor's seeded rate
/// for the same model name.
fn gateway_alias_rate(model: &str) -> (Option<(f64, f64)>, String) {
    let Some((vendor, vendor_model)) = model.split_once('/') else {
        return (None, format!("unpriced: {model} carries no vendor prefix"));
    };
    match seeded_model_rate(vendor, vendor_model).and_then(parse_rate) {
        Some(rate) => {
            (Some(rate), format!("catalog seed {vendor}/{vendor_model}, USD per 1M tokens"))
        }
        None => (None, format!("unpriced: {vendor}/{vendor_model} not seeded")),
    }
}

/// Input and output tokens of one usage payload, in either the chat
/// (`prompt_tokens`/`completion_tokens`) or the SystemOne
/// (`input_tokens`/`output_tokens`) vocabulary.
#[must_use]
pub(crate) fn usage_tokens(usage: &serde_json::Value) -> (u64, u64) {
    let field = |names: &[&str]| {
        names.iter().find_map(|name| usage.get(*name).and_then(serde_json::Value::as_u64))
    };
    (
        field(&["input_tokens", "prompt_tokens"]).unwrap_or(0),
        field(&["output_tokens", "completion_tokens"]).unwrap_or(0),
    )
}

/// Nearest-rank percentile: the smallest value with at least `quantile` of the
/// sorted sample at or below it.
#[must_use]
pub(crate) fn percentile(sorted: &[u128], quantile: f64) -> Option<u128> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (quantile * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted.get(rank - 1).copied()
}

/// `{p50, p95, max}` of an unsorted latency sample in milliseconds.
#[must_use]
pub(crate) fn latency_summary(latencies_ms: &[u128]) -> serde_json::Value {
    let mut sorted = latencies_ms.to_vec();
    sorted.sort_unstable();
    serde_json::json!({
        "p50": percentile(&sorted, 0.50),
        "p95": percentile(&sorted, 0.95),
        "max": sorted.last(),
    })
}

#[test]
fn arm_spec_carries_effort_and_service_tier() {
    let arm = litellm_arm(
        "openai/gpt-6-luna@none~flex",
        AiBindingPurpose::ExtractGraph,
        "http://127.0.0.1:9/v1",
        "key",
    );
    assert_eq!(arm.name, "litellm/openai/gpt-6-luna@none~flex");
    assert_eq!(arm.binding.model_name, "openai/gpt-6-luna");
    assert_eq!(arm.binding.extra_parameters_json["reasoning_effort"], "none");
    assert_eq!(arm.binding.extra_parameters_json["service_tier"], "flex");
    let standard = litellm_arm("openai/gpt-6-luna", AiBindingPurpose::ExtractGraph, "x", "key");
    let (flex, full) = (arm.rate.expect("flex is priced"), standard.rate.expect("seeded"));
    assert!((flex.0 - full.0 * FLEX_RATE_FACTOR).abs() < 1e-12);
    assert!((flex.1 - full.1 * FLEX_RATE_FACTOR).abs() < 1e-12);
}

#[test]
fn percentile_uses_the_nearest_rank() {
    assert_eq!(percentile(&[], 0.5), None);
    assert_eq!(percentile(&[10, 20, 30, 40], 0.5), Some(20));
    assert_eq!(percentile(&[10, 20, 30, 40], 0.95), Some(40));
    assert_eq!(
        usage_tokens(&serde_json::json!({"prompt_tokens": 3, "completion_tokens": 4})),
        (3, 4)
    );
    assert_eq!(usage_tokens(&serde_json::json!({"input_tokens": 5, "output_tokens": 6})), (5, 6));
}
