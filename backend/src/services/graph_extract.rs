use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    app::state::AppState,
    domains::{provider_profiles::EffectiveProviderProfile, runtime_graph::RuntimeNodeType},
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphExtractionRecordRow},
    integrations::llm::{ChatRequest, LlmGateway},
};

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract_v1";
const GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS: usize = 2;

#[derive(Debug, Clone)]
pub struct GraphExtractionRequest {
    pub project_id: uuid::Uuid,
    pub document: DocumentRow,
    pub chunk: ChunkRow,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionLifecycle {
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEntityCandidate {
    pub label: String,
    pub node_type: RuntimeNodeType,
    pub aliases: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphRelationCandidate {
    pub source_label: String,
    pub target_label: String,
    pub relation_type: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionCandidateSet {
    pub entities: Vec<GraphEntityCandidate>,
    pub relations: Vec<GraphRelationCandidate>,
}

#[derive(Debug, Clone)]
pub struct GraphExtractionOutcome {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt_hash: String,
    pub raw_output_json: serde_json::Value,
    pub usage_json: serde_json::Value,
    pub usage_calls: Vec<GraphExtractionUsageCall>,
    pub normalized: GraphExtractionCandidateSet,
    pub lifecycle: GraphExtractionLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExtractionUsageCall {
    pub provider_call_no: i32,
    pub provider_attempt_no: i32,
    pub prompt_hash: String,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
struct RawGraphExtractionResponse {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    output_text: String,
    usage_json: serde_json::Value,
    lifecycle: GraphExtractionLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphExtractionRecoveryAttempt {
    provider_attempt_no: usize,
    prompt_hash: String,
    output_text: String,
    usage_json: serde_json::Value,
    parse_error: Option<String>,
    normalization_path: String,
    repair_candidate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GraphExtractionRecoveryTrace {
    provider_attempt_count: usize,
    reask_count: usize,
    local_repair_applied: bool,
    attempts: Vec<GraphExtractionRecoveryAttempt>,
}

#[derive(Debug, Clone)]
struct ResolvedGraphExtraction {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    output_text: String,
    usage_json: serde_json::Value,
    usage_calls: Vec<GraphExtractionUsageCall>,
    normalized: GraphExtractionCandidateSet,
    lifecycle: GraphExtractionLifecycle,
    recovery: GraphExtractionRecoveryTrace,
}

#[derive(Debug, Clone)]
struct GraphExtractionFailureOutcome {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    provider_attempt_count: usize,
    raw_output_json: serde_json::Value,
    error_message: String,
}

#[must_use]
pub fn build_graph_extraction_prompt(request: &GraphExtractionRequest) -> String {
    format!(
        "You extract graph-ready entities and relationships from a document chunk.\n\
Return strict JSON with keys `entities` and `relations`.\n\
Each entity object must include: label, node_type (`entity` or `topic`), aliases, summary.\n\
Each relation object must include: source_label, target_label, relation_type, summary.\n\
Do not include markdown fences.\n\
If no graph evidence exists, return {{\"entities\":[],\"relations\":[]}}.\n\
\nDocument: {}\nChunk ordinal: {}\nChunk text:\n{}",
        request
            .document
            .title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&request.document.external_key),
        request.chunk.ordinal,
        request.chunk.content
    )
}

pub async fn extract_chunk_graph(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> Result<GraphExtractionOutcome> {
    let resolved = resolve_graph_extraction(state, provider_profile, request)
        .await
        .map_err(|failure| anyhow!(failure.error_message))?;

    Ok(GraphExtractionOutcome {
        provider_kind: resolved.provider_kind.clone(),
        model_name: resolved.model_name.clone(),
        prompt_hash: resolved.prompt_hash.clone(),
        raw_output_json: build_raw_output_json(
            &resolved.output_text,
            resolved.usage_json.clone(),
            &resolved.lifecycle,
            &resolved.recovery,
        ),
        usage_json: resolved.usage_json.clone(),
        usage_calls: resolved.usage_calls,
        normalized: resolved.normalized,
        lifecycle: resolved.lifecycle,
    })
}

pub async fn extract_and_persist_chunk_graph(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> Result<RuntimeGraphExtractionRecordRow> {
    match resolve_graph_extraction(state, provider_profile, request).await {
        Ok(resolved) => repositories::create_runtime_graph_extraction_record(
            &state.persistence.postgres,
            request.project_id,
            request.document.id,
            request.chunk.id,
            &resolved.provider_kind,
            &resolved.model_name,
            GRAPH_EXTRACTION_VERSION,
            &resolved.prompt_hash,
            "ready",
            build_raw_output_json(
                &resolved.output_text,
                resolved.usage_json,
                &resolved.lifecycle,
                &resolved.recovery,
            ),
            serde_json::to_value(resolved.normalized).unwrap_or_else(|_| serde_json::json!({})),
            i32::try_from(resolved.recovery.provider_attempt_count).unwrap_or(i32::MAX),
            None,
        )
        .await
        .context("failed to persist graph extraction record"),
        Err(failure) => repositories::create_runtime_graph_extraction_record(
            &state.persistence.postgres,
            request.project_id,
            request.document.id,
            request.chunk.id,
            &failure.provider_kind,
            &failure.model_name,
            GRAPH_EXTRACTION_VERSION,
            &failure.prompt_hash,
            "failed",
            failure.raw_output_json,
            serde_json::json!({ "entities": [], "relations": [] }),
            i32::try_from(failure.provider_attempt_count).unwrap_or(i32::MAX),
            Some(&failure.error_message),
        )
        .await
        .context("failed to persist graph extraction failure record"),
    }
}

pub async fn extract_and_persist_chunk_graph_result(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> Result<GraphExtractionOutcome> {
    match resolve_graph_extraction(state, provider_profile, request).await {
        Ok(resolved) => {
            let outcome = GraphExtractionOutcome {
                provider_kind: resolved.provider_kind.clone(),
                model_name: resolved.model_name.clone(),
                prompt_hash: resolved.prompt_hash.clone(),
                raw_output_json: build_raw_output_json(
                    &resolved.output_text,
                    resolved.usage_json.clone(),
                    &resolved.lifecycle,
                    &resolved.recovery,
                ),
                usage_json: resolved.usage_json.clone(),
                usage_calls: resolved.usage_calls.clone(),
                normalized: resolved.normalized,
                lifecycle: resolved.lifecycle,
            };
            let raw_output_json = outcome.raw_output_json.clone();
            let normalized_json =
                serde_json::to_value(&outcome.normalized).unwrap_or_else(|_| serde_json::json!({}));
            repositories::create_runtime_graph_extraction_record(
                &state.persistence.postgres,
                request.project_id,
                request.document.id,
                request.chunk.id,
                &outcome.provider_kind,
                &outcome.model_name,
                GRAPH_EXTRACTION_VERSION,
                &outcome.prompt_hash,
                "ready",
                raw_output_json,
                normalized_json,
                i32::try_from(resolved.recovery.provider_attempt_count).unwrap_or(i32::MAX),
                None,
            )
            .await
            .context("failed to persist graph extraction record")?;
            Ok(outcome)
        }
        Err(failure) => {
            repositories::create_runtime_graph_extraction_record(
                &state.persistence.postgres,
                request.project_id,
                request.document.id,
                request.chunk.id,
                &failure.provider_kind,
                &failure.model_name,
                GRAPH_EXTRACTION_VERSION,
                &failure.prompt_hash,
                "failed",
                failure.raw_output_json,
                serde_json::json!({ "entities": [], "relations": [] }),
                i32::try_from(failure.provider_attempt_count).unwrap_or(i32::MAX),
                Some(&failure.error_message),
            )
            .await
            .context("failed to persist graph extraction failure record")?;
            Err(anyhow!(failure.error_message))
        }
    }
}

#[must_use]
pub fn extraction_lifecycle_from_record(
    record: &RuntimeGraphExtractionRecordRow,
) -> GraphExtractionLifecycle {
    record
        .raw_output_json
        .get("lifecycle")
        .and_then(|value| serde_json::from_value::<GraphExtractionLifecycle>(value.clone()).ok())
        .unwrap_or_default()
}

async fn resolve_graph_extraction(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    resolve_graph_extraction_with_gateway(
        state.llm_gateway.as_ref(),
        provider_profile,
        request,
        state.retrieval_intelligence.extraction_recovery_enabled,
        state
            .retrieval_intelligence
            .extraction_recovery_max_attempts
            .clamp(1, GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
    )
    .await
}

async fn resolve_graph_extraction_with_gateway(
    gateway: &dyn LlmGateway,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
    recovery_enabled: bool,
    max_provider_attempts: usize,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let provider_kind = provider_profile.indexing.provider_kind.as_str().to_string();
    let model_name = provider_profile.indexing.model_name.clone();
    let lifecycle = GraphExtractionLifecycle {
        revision_id: request.revision_id,
        activated_by_attempt_id: request.activated_by_attempt_id,
    };
    let mut trace = GraphExtractionRecoveryTrace::default();
    let mut usage_samples = Vec::new();
    let mut usage_calls = Vec::new();
    let mut previous_invalid_output = None;
    let mut previous_parse_error = None;

    let max_provider_attempts = if recovery_enabled { max_provider_attempts.max(1) } else { 1 };
    for provider_attempt_no in 1..=max_provider_attempts {
        let raw = match provider_attempt_no {
            1 => request_graph_extraction(gateway, provider_profile, request).await,
            _ => {
                request_graph_extraction_retry(
                    gateway,
                    provider_profile,
                    request,
                    previous_invalid_output.as_deref().unwrap_or_default(),
                    previous_parse_error.as_deref().unwrap_or("unknown parse failure"),
                )
                .await
            }
        };
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                return Err(GraphExtractionFailureOutcome {
                    provider_kind: provider_kind.clone(),
                    model_name: model_name.clone(),
                    prompt_hash: sha256_hex(&build_graph_extraction_prompt(request)),
                    provider_attempt_count: trace.provider_attempt_count,
                    raw_output_json: serde_json::json!({
                        "output_text": previous_invalid_output,
                        "usage": aggregate_provider_usage_json(&provider_kind, &model_name, &usage_samples),
                        "lifecycle": lifecycle,
                        "recovery": trace,
                    }),
                    error_message: if provider_attempt_no == 1 {
                        format!(
                            "graph extraction provider call failed before normalization retry: {error:#}"
                        )
                    } else {
                        format!(
                            "graph extraction retry attempt {} failed after malformed output: {}; provider error: {error:#}",
                            provider_attempt_no,
                            previous_parse_error.as_deref().unwrap_or("unknown parse failure"),
                        )
                    },
                });
            }
        };
        usage_samples.push(raw.usage_json.clone());
        usage_calls.push(GraphExtractionUsageCall {
            provider_call_no: i32::try_from(usage_calls.len() + 1).unwrap_or(i32::MAX),
            provider_attempt_no: i32::try_from(provider_attempt_no).unwrap_or(i32::MAX),
            prompt_hash: raw.prompt_hash.clone(),
            usage_json: raw.usage_json.clone(),
        });
        match normalize_graph_extraction_output_with_repair(&raw.output_text) {
            Ok((normalized, normalization_path, repair_candidate)) => {
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                trace.local_repair_applied |= normalization_path == "repaired";
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    parse_error: None,
                    normalization_path: normalization_path.to_string(),
                    repair_candidate,
                });
                return Ok(ResolvedGraphExtraction {
                    provider_kind: raw.provider_kind.clone(),
                    model_name: raw.model_name.clone(),
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: aggregate_provider_usage_json(
                        &raw.provider_kind,
                        &raw.model_name,
                        &usage_samples,
                    ),
                    usage_calls,
                    normalized,
                    lifecycle: raw.lifecycle.clone(),
                    recovery: trace,
                });
            }
            Err(parse_error) => {
                let parse_error = parse_error.to_string();
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    parse_error: Some(parse_error.clone()),
                    normalization_path: "failed".to_string(),
                    repair_candidate: repair_graph_extraction_output(&raw.output_text),
                });
                previous_invalid_output = Some(raw.output_text.clone());
                previous_parse_error = Some(parse_error.clone());
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if provider_attempt_no == max_provider_attempts {
                    let provider_attempt_count = trace.provider_attempt_count;
                    return Err(GraphExtractionFailureOutcome {
                        provider_kind: raw.provider_kind.clone(),
                        model_name: raw.model_name.clone(),
                        prompt_hash: raw.prompt_hash.clone(),
                        provider_attempt_count: trace.provider_attempt_count,
                        raw_output_json: build_raw_output_json(
                            &raw.output_text,
                            aggregate_provider_usage_json(
                                &raw.provider_kind,
                                &raw.model_name,
                                &usage_samples,
                            ),
                            &raw.lifecycle,
                            &trace,
                        ),
                        error_message: format!(
                            "failed to normalize graph extraction output after {} provider attempt(s): {}",
                            provider_attempt_count, parse_error,
                        ),
                    });
                }
            }
        }
    }

    let aggregate_usage =
        aggregate_provider_usage_json(&provider_kind, &model_name, &usage_samples);
    Err(GraphExtractionFailureOutcome {
        provider_kind,
        model_name,
        prompt_hash: sha256_hex(&build_graph_extraction_prompt(request)),
        provider_attempt_count: trace.provider_attempt_count,
        raw_output_json: serde_json::json!({
            "usage": aggregate_usage,
            "lifecycle": lifecycle,
            "recovery": trace,
        }),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
    })
}

async fn request_graph_extraction(
    gateway: &dyn LlmGateway,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> Result<RawGraphExtractionResponse> {
    request_graph_extraction_with_prompt(
        gateway,
        provider_profile,
        build_graph_extraction_prompt(request),
        GraphExtractionLifecycle {
            revision_id: request.revision_id,
            activated_by_attempt_id: request.activated_by_attempt_id,
        },
    )
    .await
}

async fn request_graph_extraction_retry(
    gateway: &dyn LlmGateway,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
    previous_invalid_output: &str,
    parse_error: &str,
) -> Result<RawGraphExtractionResponse> {
    request_graph_extraction_with_prompt(
        gateway,
        provider_profile,
        build_graph_extraction_retry_prompt(request, previous_invalid_output, parse_error),
        GraphExtractionLifecycle {
            revision_id: request.revision_id,
            activated_by_attempt_id: request.activated_by_attempt_id,
        },
    )
    .await
}

async fn request_graph_extraction_with_prompt(
    gateway: &dyn LlmGateway,
    provider_profile: &EffectiveProviderProfile,
    prompt: String,
    lifecycle: GraphExtractionLifecycle,
) -> Result<RawGraphExtractionResponse> {
    let prompt_hash = sha256_hex(&prompt);
    let provider_kind = provider_profile.indexing.provider_kind.as_str().to_string();
    let model_name = provider_profile.indexing.model_name.clone();
    let response = gateway
        .generate(ChatRequest {
            provider_kind: provider_kind.clone(),
            model_name: model_name.clone(),
            prompt,
        })
        .await
        .context("graph extraction provider call failed")?;

    Ok(RawGraphExtractionResponse {
        provider_kind,
        model_name,
        prompt_hash,
        output_text: response.output_text,
        usage_json: build_provider_usage_json(
            provider_profile.indexing.provider_kind.as_str(),
            &provider_profile.indexing.model_name,
            response.usage_json,
        ),
        lifecycle,
    })
}

fn build_graph_extraction_retry_prompt(
    request: &GraphExtractionRequest,
    previous_invalid_output: &str,
    parse_error: &str,
) -> String {
    format!(
        "The previous graph extraction output was malformed and could not be normalized.\n\
Normalization error: {parse_error}\n\
Return only strict JSON with keys `entities` and `relations`.\n\
Do not include any prose or markdown fences.\n\
\nPrevious invalid output:\n{previous_invalid_output}\n\
\nRetry the same extraction task.\n\n{}",
        build_graph_extraction_prompt(request)
    )
}

#[must_use]
pub fn extraction_provider_usage_json(
    record: &RuntimeGraphExtractionRecordRow,
) -> serde_json::Value {
    record.raw_output_json.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}))
}

fn build_raw_output_json(
    output_text: &str,
    usage_json: serde_json::Value,
    lifecycle: &GraphExtractionLifecycle,
    recovery: &GraphExtractionRecoveryTrace,
) -> serde_json::Value {
    serde_json::json!({
        "output_text": output_text,
        "usage": usage_json,
        "lifecycle": lifecycle,
        "recovery": recovery,
    })
}

fn build_provider_usage_json(
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

fn aggregate_provider_usage_json(
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

    serde_json::json!({
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
    })
}

fn normalize_graph_extraction_output_with_repair(
    output_text: &str,
) -> Result<(GraphExtractionCandidateSet, &'static str, Option<String>)> {
    match parse_graph_extraction_output(output_text) {
        Ok(normalized) => Ok((normalized, "direct", None)),
        Err(primary_error) => {
            let repair_candidate = repair_graph_extraction_output(output_text);
            if let Some(candidate) = &repair_candidate {
                if let Ok(normalized) = parse_graph_extraction_output(candidate) {
                    return Ok((normalized, "repaired", Some(candidate.clone())));
                }
            }
            Err(primary_error)
        }
    }
}

fn repair_graph_extraction_output(output_text: &str) -> Option<String> {
    let normalized = normalize_jsonish_text(output_text);
    let mut candidates = Vec::new();
    if let Some(candidate) = synthesize_root_object_from_sections(&normalized) {
        candidates.push(candidate);
    }
    if normalized != output_text.trim() {
        candidates.push(normalized.clone());
    }
    if candidates.is_empty() { None } else { Some(candidates.remove(0)) }
}

fn normalize_jsonish_text(value: &str) -> String {
    value
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
        .replace('\u{00A0}', " ")
        .replace('\u{200B}', "")
        .trim()
        .to_string()
}

fn synthesize_root_object_from_sections(value: &str) -> Option<String> {
    let entities =
        extract_named_array_fragment(value, "entities").unwrap_or_else(|| "[]".to_string());
    let relations =
        extract_named_array_fragment(value, "relations").unwrap_or_else(|| "[]".to_string());
    if entities == "[]" && relations == "[]" {
        return None;
    }
    Some(format!("{{\"entities\":{entities},\"relations\":{relations}}}"))
}

fn extract_named_array_fragment(value: &str, key: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let needle = key.to_ascii_lowercase();
    let key_index = lower.find(&needle)?;
    let array_start = value[key_index..].find('[')? + key_index;
    let array_end = find_matching_bracket(value, array_start)?;
    Some(value[array_start..=array_end].trim().to_string())
}

fn find_matching_bracket(value: &str, start_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices().skip_while(|(index, _)| *index < start_index) {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse_graph_extraction_output(output_text: &str) -> Result<GraphExtractionCandidateSet> {
    let parsed = extract_json_payload(output_text)?;
    let entities = parsed
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_entity_candidate).collect::<Vec<_>>())
        .unwrap_or_default();
    let relations = parsed
        .get("relations")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_relation_candidate).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(GraphExtractionCandidateSet { entities, relations })
}

fn parse_entity_candidate(value: &serde_json::Value) -> Option<GraphEntityCandidate> {
    if let Some(label) = value.as_str().map(str::trim).filter(|value| !value.is_empty()) {
        return Some(GraphEntityCandidate {
            label: label.to_string(),
            node_type: RuntimeNodeType::Entity,
            aliases: Vec::new(),
            summary: None,
        });
    }

    let label = value.get("label").and_then(serde_json::Value::as_str)?.trim();
    if label.is_empty() {
        return None;
    }
    let node_type = match value
        .get("node_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("entity")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "topic" => RuntimeNodeType::Topic,
        _ => RuntimeNodeType::Entity,
    };
    let aliases = value
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(GraphEntityCandidate {
        label: label.to_string(),
        node_type,
        aliases,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn parse_relation_candidate(value: &serde_json::Value) -> Option<GraphRelationCandidate> {
    let source_label = value
        .get("source_label")
        .or_else(|| value.get("source"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let target_label = value
        .get("target_label")
        .or_else(|| value.get("target"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let relation_type = value
        .get("relation_type")
        .or_else(|| value.get("type"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    if source_label.is_empty() || target_label.is_empty() || relation_type.is_empty() {
        return None;
    }
    let normalized_relation_type = normalize_relation_candidate_type(relation_type)?;
    if relation_type_is_semantically_void(&normalized_relation_type) {
        return None;
    }

    Some(GraphRelationCandidate {
        source_label: source_label.to_string(),
        target_label: target_label.to_string(),
        relation_type: normalized_relation_type,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn normalize_relation_candidate_type(relation_type: &str) -> Option<String> {
    let normalized = relation_type
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() { char } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    (!normalized.is_empty()).then_some(normalized)
}

fn relation_type_is_semantically_void(normalized_relation_type: &str) -> bool {
    matches!(
        normalized_relation_type,
        "na" | "n_a"
            | "none"
            | "null"
            | "unknown"
            | "unspecified"
            | "tbd"
            | "relation"
            | "relationship"
    )
}

fn extract_json_payload(output_text: &str) -> Result<serde_json::Value> {
    let trimmed = output_text.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({ "entities": [], "relations": [] }));
    }

    let unfenced = strip_markdown_fence(trimmed);
    let mut candidates = vec![unfenced.to_string()];
    if let Some(object_candidate) = extract_outer_json_object(unfenced) {
        if object_candidate != unfenced {
            candidates.push(object_candidate);
        }
    }

    let repaired_candidates = candidates
        .iter()
        .filter_map(|candidate| {
            let repaired = close_unbalanced_json_containers(candidate);
            (repaired != *candidate).then_some(repaired)
        })
        .collect::<Vec<_>>();
    candidates.extend(repaired_candidates);

    let mut parse_errors = Vec::new();
    for candidate in candidates {
        match serde_json::from_str::<serde_json::Value>(&candidate) {
            Ok(value) => return Ok(value),
            Err(json_error) => {
                parse_errors.push(format!("strict json: {json_error}"));
            }
        }
        match json5::from_str::<serde_json::Value>(&candidate) {
            Ok(value) => return Ok(value),
            Err(json5_error) => {
                parse_errors.push(format!("json5 fallback: {json5_error}"));
            }
        }
    }

    Err(anyhow!(
        "invalid graph extraction json: {}",
        if parse_errors.is_empty() {
            "unknown parse failure".to_string()
        } else {
            parse_errors.join(" | ")
        }
    ))
}

fn strip_markdown_fence(value: &str) -> &str {
    if !value.starts_with("```") {
        return value;
    }

    value
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

fn extract_outer_json_object(value: &str) -> Option<String> {
    let start = value.find('{')?;
    let end = value.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(value[start..=end].trim().to_string())
}

fn close_unbalanced_json_containers(value: &str) -> String {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }

        match ch {
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.last().copied() == Some(ch) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }

    let mut repaired = value.trim_end().to_string();
    while let Some(ch) = stack.pop() {
        repaired.push(ch);
    }
    repaired
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::{
        domains::provider_profiles::{ProviderModelSelection, SupportedProviderKind},
        integrations::llm::{
            ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse,
            EmbeddingRequest, EmbeddingResponse, VisionRequest, VisionResponse,
        },
    };

    struct FakeGateway {
        responses: Mutex<Vec<Result<ChatResponse>>>,
    }

    #[async_trait]
    impl LlmGateway for FakeGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            self.responses.lock().expect("lock fake responses").remove(0)
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in graph extraction tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in graph extraction tests")
        }

        async fn vision_extract(&self, _request: VisionRequest) -> Result<VisionResponse> {
            unreachable!("vision_extract is not used in graph extraction tests")
        }
    }

    fn sample_document() -> DocumentRow {
        DocumentRow {
            id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            source_id: None,
            external_key: "spec.md".to_string(),
            title: Some("Spec".to_string()),
            mime_type: Some("text/markdown".to_string()),
            checksum: None,
            current_revision_id: None,
            active_status: "active".to_string(),
            active_mutation_kind: None,
            active_mutation_status: None,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_chunk() -> ChunkRow {
        ChunkRow {
            id: uuid::Uuid::nil(),
            document_id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            ordinal: 0,
            content: "OpenAI supplies embeddings for the annual report graph.".to_string(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn sample_profile() -> EffectiveProviderProfile {
        EffectiveProviderProfile {
            indexing: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
            embedding: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "text-embedding-3-small".to_string(),
            },
            answer: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5.4".to_string(),
            },
            vision: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
        }
    }

    fn sample_request() -> GraphExtractionRequest {
        GraphExtractionRequest {
            project_id: uuid::Uuid::nil(),
            document: sample_document(),
            chunk: sample_chunk(),
            revision_id: None,
            activated_by_attempt_id: None,
        }
    }

    #[test]
    fn prompt_mentions_json_contract_and_chunk_text() {
        let prompt = build_graph_extraction_prompt(&GraphExtractionRequest {
            project_id: uuid::Uuid::nil(),
            document: sample_document(),
            chunk: sample_chunk(),
            revision_id: None,
            activated_by_attempt_id: None,
        });

        assert!(prompt.contains("strict JSON"));
        assert!(prompt.contains("entities"));
        assert!(prompt.contains("annual report graph"));
    }

    #[test]
    fn normalizes_json_and_string_candidates() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                "Annual report",
                { "label": "OpenAI", "node_type": "topic", "aliases": ["Open AI"], "summary": "provider" }
              ],
              "relations": [
                { "source": "Annual report", "target": "OpenAI", "type": "mentions" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.entities.len(), 2);
        assert_eq!(normalized.entities[0].label, "Annual report");
        assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Topic);
        assert_eq!(normalized.relations[0].relation_type, "mentions");
    }

    #[test]
    fn parses_json_inside_markdown_fence() {
        let normalized =
            parse_graph_extraction_output("```json\n{\"entities\":[],\"relations\":[]}\n```")
                .expect("normalize fenced graph extraction");

        assert!(normalized.entities.is_empty());
        assert!(normalized.relations.is_empty());
    }

    #[test]
    fn drops_empty_candidates_and_normalizes_relation_labels() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                { "label": "  ", "node_type": "entity" },
                { "label": "DeepSeek", "aliases": ["", " Deep Seek "] }
              ],
              "relations": [
                { "source_label": "DeepSeek", "target_label": "Knowledge Graph", "relation_type": "Builds On" },
                { "source_label": " ", "target_label": "Ignored", "relation_type": "mentions" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].label, "DeepSeek");
        assert_eq!(normalized.entities[0].aliases, vec!["Deep Seek".to_string()]);
        assert_eq!(normalized.relations.len(), 1);
        assert_eq!(normalized.relations[0].relation_type, "builds_on");
    }

    #[test]
    fn drops_semantically_void_relation_types_at_parse_time() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [],
              "relations": [
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "unknown" },
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.relations.len(), 1);
        assert_eq!(normalized.relations[0].relation_type, "supports");
    }

    #[test]
    fn extracts_json_object_from_surrounding_prose() {
        let normalized = parse_graph_extraction_output(
            "Here is the result:\n{\"entities\":[\"OpenAI\"],\"relations\":[]}\nThanks.",
        )
        .expect("normalize graph extraction with prose");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].label, "OpenAI");
    }

    #[test]
    fn accepts_json5_style_payloads() {
        let normalized = parse_graph_extraction_output(
            "{entities:[{label:'OpenAI', node_type:'entity', aliases:['Open AI'], summary:'provider',},], relations:[]}",
        )
        .expect("normalize json5 graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].aliases, vec!["Open AI".to_string()]);
    }

    #[test]
    fn closes_missing_json_containers_at_end() {
        let normalized = parse_graph_extraction_output(
            r#"{"entities":[{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}],"relations":[{"source_label":"OpenAI","target_label":"Graph","relation_type":"mentions","summary":"link"}"#,
        )
        .expect("normalize truncated graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.relations.len(), 1);
    }

    #[test]
    fn rejects_non_json_payloads() {
        let error = parse_graph_extraction_output("not valid json").expect_err("invalid json");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn repairs_named_sections_without_outer_object() {
        let (normalized, path, repair_candidate) = normalize_graph_extraction_output_with_repair(
            r#"
            entities:
            [{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}]
            relations:
            [{"source_label":"OpenAI","target_label":"Annual report","relation_type":"mentions","summary":"citation"}]
            "#,
        )
        .expect("repair malformed extraction payload");

        assert_eq!(path, "repaired");
        assert!(repair_candidate.is_some());
        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.relations.len(), 1);
    }

    #[tokio::test]
    async fn retries_after_terminal_parse_failure_and_aggregates_usage() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "this is not json".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 11,
                        "completion_tokens": 4,
                        "total_tokens": 15,
                    }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 7,
                        "completion_tokens": 3,
                        "total_tokens": 10,
                    }),
                }),
            ]),
        };

        let resolved = resolve_graph_extraction_with_gateway(
            &gateway,
            &sample_profile(),
            &sample_request(),
            true,
            2,
        )
        .await
        .expect("retry should recover");

        assert_eq!(resolved.recovery.provider_attempt_count, 2);
        assert_eq!(resolved.recovery.reask_count, 1);
        assert_eq!(
            resolved.usage_json.get("call_count").and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            resolved.usage_json.get("total_tokens").and_then(serde_json::Value::as_i64),
            Some(25)
        );
    }

    #[tokio::test]
    async fn fails_after_retry_exhaustion_with_recovery_trace() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "broken payload".to_string(),
                    usage_json: serde_json::json!({ "prompt_tokens": 5 }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "still broken".to_string(),
                    usage_json: serde_json::json!({ "prompt_tokens": 6 }),
                }),
            ]),
        };

        let failure = resolve_graph_extraction_with_gateway(
            &gateway,
            &sample_profile(),
            &sample_request(),
            true,
            2,
        )
        .await
        .expect_err("malformed output should fail after retry exhaustion");

        assert!(failure.error_message.contains("after 2 provider attempt(s)"));
        assert_eq!(failure.provider_attempt_count, 2);
        assert_eq!(
            failure
                .raw_output_json
                .get("recovery")
                .and_then(|value| value.get("attempts"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn provider_usage_payload_keeps_provider_metadata() {
        let usage = build_provider_usage_json(
            "openai",
            "gpt-5-mini",
            serde_json::json!({
                "prompt_tokens": 21,
                "completion_tokens": 9,
            }),
        );

        assert_eq!(usage.get("provider_kind").and_then(serde_json::Value::as_str), Some("openai"));
        assert_eq!(usage.get("model_name").and_then(serde_json::Value::as_str), Some("gpt-5-mini"));
        assert_eq!(usage.get("prompt_tokens").and_then(serde_json::Value::as_i64), Some(21));
    }
}
