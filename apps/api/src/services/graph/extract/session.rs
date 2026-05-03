use anyhow::{Context, Result};
use chrono::Utc;
use std::time::Instant;

use crate::{
    agent_runtime::{
        request::build_provider_request, task::RuntimeTask, tasks::graph_extract::GraphExtractTask,
    },
    domains::{
        graph_quality::ExtractionRecoverySummary,
        provider_profiles::EffectiveProviderProfile,
        runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    },
    integrations::llm::{ChatRequestSeed, LlmGateway},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::extraction_recovery::ExtractionRecoveryService,
    },
};

use super::graph_extraction_cache_hash;
use super::parse::{normalize_graph_extraction_output, sanitize_graph_extraction_candidate_set};
use super::prompt::build_graph_extraction_prompt_plan;
use super::types::*;

pub(crate) async fn resolve_graph_extraction_with_gateway(
    gateway: &dyn LlmGateway,
    extraction_recovery: &ExtractionRecoveryService,
    provider_failure_classification: &crate::services::ops::provider_failure::ProviderFailureClassificationService,
    provider_profile: &EffectiveProviderProfile,
    runtime_binding: &ResolvedRuntimeBinding,
    request: &GraphExtractionRequest,
    recovery_enabled: bool,
    max_provider_attempts: usize,
    provider_timeout_retry_limit: usize,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let lifecycle = GraphExtractionLifecycle {
        revision_id: request.revision_id,
        activated_by_attempt_id: request.activated_by_attempt_id,
    };
    let mut trace = GraphExtractionRecoveryTrace::default();
    let mut usage_samples = Vec::new();
    let mut usage_calls = Vec::new();
    let mut pending_follow_up = None;
    let mut pending_recovery_records = Vec::new();
    let mut best_partial_candidate = None;
    let request_size_soft_limit_bytes =
        provider_failure_classification.request_size_soft_limit_bytes();

    let max_provider_attempts = if recovery_enabled { max_provider_attempts.max(1) } else { 1 };
    for provider_attempt_no in 1..=max_provider_attempts {
        let retry_decision = (provider_attempt_no > 1).then_some("retrying_provider_call");
        let prompt_plan = match pending_follow_up.take() {
            None => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::Initial,
                None,
                None,
                None,
                request_size_soft_limit_bytes,
            ),
            Some(RecoveryFollowUpRequest::ProviderRetry {
                trigger_reason,
                issue_summary,
                previous_output,
            }) => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::ProviderRetry,
                Some(&trigger_reason),
                Some(&issue_summary),
                Some(&previous_output),
                request_size_soft_limit_bytes,
            ),
            Some(RecoveryFollowUpRequest::SecondPass {
                trigger_reason,
                issue_summary,
                previous_output,
            }) => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::SecondPass,
                Some(&trigger_reason),
                Some(&issue_summary),
                Some(&previous_output),
                request_size_soft_limit_bytes,
            ),
        };
        let raw = match request_graph_extraction_with_prompt_plan(
            gateway,
            provider_profile,
            runtime_binding,
            &prompt_plan,
            lifecycle.clone(),
        )
        .await
        {
            Ok(raw) => raw,
            Err(error) => {
                let error_context = format!("{error:#}");
                let provider_failure = provider_failure_classification.classify_failure(
                    &provider_kind,
                    &model_name,
                    &error_context,
                    &prompt_plan.request_shape_key,
                    prompt_plan.request_size_bytes,
                    Some(1),
                    None,
                    retry_decision.map(str::to_string),
                    !usage_calls.is_empty(),
                );
                let transient_retry_plan = if provider_failure_classification
                    .is_transient_retryable_failure(&provider_failure)
                {
                    match provider_failure.failure_class {
                        RuntimeProviderFailureClass::UpstreamTimeout => Some((
                            "upstream_timeout",
                            "Retrying graph extraction after an upstream timeout.",
                        )),
                        RuntimeProviderFailureClass::UpstreamProtocolFailure => Some((
                            "upstream_protocol_failure",
                            "Retrying graph extraction after an upstream protocol parse failure on a locally valid request.",
                        )),
                        RuntimeProviderFailureClass::UpstreamRejection => Some((
                            "upstream_transient_rejection",
                            "Retrying graph extraction after a transient upstream rejection.",
                        )),
                        _ => None,
                    }
                } else {
                    None
                };
                let allow_transient_retry = transient_retry_plan.is_some()
                    && provider_attempt_no <= provider_timeout_retry_limit
                    && provider_attempt_no < max_provider_attempts;
                if let (true, Some((trigger_reason, recovered_summary))) =
                    (allow_transient_retry, transient_retry_plan)
                {
                    let raw_issue_summary =
                        extraction_recovery.redact_recovery_summary(&error_context);
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: trigger_reason.to_string(),
                        raw_issue_summary: Some(raw_issue_summary.clone()),
                        recovered_summary: Some(
                            extraction_recovery.redact_recovery_summary(recovered_summary),
                        ),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: trigger_reason.to_string(),
                        issue_summary: raw_issue_summary,
                        previous_output: String::new(),
                    });
                    trace.provider_attempt_count = provider_attempt_no;
                    trace.reask_count = provider_attempt_no.saturating_sub(1);
                    continue;
                }
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if let Some(candidate) = best_partial_candidate.clone() {
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        pending_recovery_records.iter().any(|record: &PendingRecoveryRecord| {
                            record.recovery_kind == "second_pass"
                        }),
                        true,
                        false,
                    );
                    let recovery_attempts = finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    );
                    return Ok(build_resolved_extraction_from_candidate(
                        candidate,
                        &provider_kind,
                        &model_name,
                        &usage_samples,
                        usage_calls,
                        prompt_plan.request_shape_key.clone(),
                        prompt_plan.request_size_bytes,
                        Some(provider_failure),
                        trace,
                        recovery_summary,
                        recovery_attempts,
                    ));
                }
                let recovery_summary = extraction_recovery.classify_outcome(
                    trace.provider_attempt_count,
                    pending_recovery_records.iter().any(|record: &PendingRecoveryRecord| {
                        record.recovery_kind == "second_pass"
                    }),
                    false,
                    true,
                );
                return Err(GraphExtractionFailureOutcome {
                    request_shape_key: prompt_plan.request_shape_key,
                    request_size_bytes: prompt_plan.request_size_bytes,
                    error_message: if provider_attempt_no == 1 {
                        format!(
                            "graph extraction provider call failed before normalization retry: {error:#}"
                        )
                    } else {
                        format!(
                            "graph extraction recovery attempt {} failed: {error:#}",
                            provider_attempt_no,
                        )
                    },
                    provider_failure: Some(provider_failure),
                    recovery_summary: recovery_summary.clone(),
                    recovery_attempts: finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    ),
                });
            }
        };
        usage_samples.push(raw.usage_json.clone());
        usage_calls.push(GraphExtractionUsageCall {
            provider_call_no: i32::try_from(usage_calls.len() + 1).unwrap_or(i32::MAX),
            provider_attempt_no: i32::try_from(provider_attempt_no).unwrap_or(i32::MAX),
            prompt_hash: raw.prompt_hash.clone(),
            request_shape_key: raw.request_shape_key.clone(),
            request_size_bytes: raw.request_size_bytes,
            usage_json: raw.usage_json.clone(),
            timing: raw.timing.clone(),
        });
        match normalize_graph_extraction_output(&raw.output_text) {
            Ok(normalized_attempt) => {
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    timing: raw.timing.clone(),
                    parse_error: None,
                    normalization_path: normalized_attempt.normalization_path.to_string(),
                    recovery_kind: None,
                    trigger_reason: None,
                });

                let second_pass = extraction_recovery.classify_second_pass(
                    &request.chunk.content,
                    normalized_attempt.normalized.entities.len(),
                    normalized_attempt.normalized.relations.len(),
                    recovery_enabled,
                    provider_attempt_no,
                    max_provider_attempts,
                );
                let current_candidate = ParsedGraphExtractionCandidate {
                    raw: raw.clone(),
                    normalized: sanitize_graph_extraction_candidate_set(
                        normalized_attempt.normalized,
                        &request.chunk.content,
                    ),
                    normalization_path: normalized_attempt.normalization_path,
                };

                if second_pass.should_attempt {
                    let second_pass_decision = second_pass.decision.clone().unwrap_or_else(|| {
                        crate::services::ingest::extraction_recovery::RecoveryDecisionSummary {
                            reason_code: "sparse_extraction".to_string(),
                            reason_summary_redacted: extraction_recovery.redact_recovery_summary(
                                "The extraction result looked too sparse for the chunk content.",
                            ),
                        }
                    });
                    best_partial_candidate = select_better_partial_candidate(
                        best_partial_candidate,
                        current_candidate.clone(),
                    );
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "second_pass".to_string(),
                        trigger_reason: second_pass_decision.reason_code.clone(),
                        raw_issue_summary: Some(second_pass_decision.reason_summary_redacted.clone()),
                        recovered_summary: Some(
                            extraction_recovery.redact_recovery_summary(
                                "Requested a second extraction pass because the first result looked sparse or inconsistent.",
                            ),
                        ),
                    });
                    trace.attempts.push(GraphExtractionRecoveryAttempt {
                        provider_attempt_no,
                        prompt_hash: raw.prompt_hash.clone(),
                        output_text: raw.output_text.clone(),
                        usage_json: raw.usage_json.clone(),
                        timing: raw.timing.clone(),
                        parse_error: None,
                        normalization_path: current_candidate.normalization_path.to_string(),
                        recovery_kind: Some("second_pass".to_string()),
                        trigger_reason: Some(second_pass_decision.reason_code.clone()),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::SecondPass {
                        trigger_reason: second_pass_decision.reason_code,
                        issue_summary: second_pass_decision.reason_summary_redacted,
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                let recovery_summary = extraction_recovery.classify_outcome(
                    trace.provider_attempt_count,
                    pending_recovery_records
                        .iter()
                        .any(|record| record.recovery_kind == "second_pass"),
                    false,
                    false,
                );
                let recovery_attempts =
                    finalize_recovery_attempt_records(&pending_recovery_records, &recovery_summary);
                return Ok(build_resolved_extraction_from_candidate(
                    current_candidate,
                    &raw.provider_kind,
                    &raw.model_name,
                    &usage_samples,
                    usage_calls,
                    raw.request_shape_key.clone(),
                    raw.request_size_bytes,
                    (provider_attempt_no > 1).then(|| {
                        provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::RecoveredAfterRetry,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("recovered_after_retry".to_string()),
                            true,
                        )
                    }),
                    trace,
                    recovery_summary,
                    recovery_attempts,
                ));
            }
            Err(parse_failure) => {
                let parse_error = parse_failure.parse_error;
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    timing: raw.timing.clone(),
                    parse_error: Some(parse_error.clone()),
                    normalization_path: "failed".to_string(),
                    recovery_kind: (provider_attempt_no < max_provider_attempts)
                        .then_some("provider_retry".to_string()),
                    trigger_reason: (provider_attempt_no < max_provider_attempts)
                        .then_some("malformed_output".to_string()),
                });
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if provider_attempt_no < max_provider_attempts {
                    let parse_error_redacted =
                        extraction_recovery.redact_recovery_summary(&parse_error);
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: "malformed_output".to_string(),
                        raw_issue_summary: Some(parse_error_redacted.clone()),
                        recovered_summary: Some(extraction_recovery.redact_recovery_summary(
                            "Requested a stricter retry after malformed extraction output.",
                        )),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: "malformed_output".to_string(),
                        issue_summary: parse_error_redacted,
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                if let Some(candidate) = best_partial_candidate.clone() {
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        pending_recovery_records
                            .iter()
                            .any(|record| record.recovery_kind == "second_pass"),
                        true,
                        false,
                    );
                    let recovery_attempts = finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    );
                    return Ok(build_resolved_extraction_from_candidate(
                        candidate,
                        &raw.provider_kind,
                        &raw.model_name,
                        &usage_samples,
                        usage_calls,
                        raw.request_shape_key.clone(),
                        raw.request_size_bytes,
                        Some(provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::RecoveredAfterRetry,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("recovered_after_retry".to_string()),
                            true,
                        )),
                        trace,
                        recovery_summary,
                        recovery_attempts,
                    ));
                }

                if provider_attempt_no == max_provider_attempts {
                    let provider_attempt_count = trace.provider_attempt_count;
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        pending_recovery_records
                            .iter()
                            .any(|record| record.recovery_kind == "second_pass"),
                        false,
                        true,
                    );
                    return Err(GraphExtractionFailureOutcome {
                        request_shape_key: raw.request_shape_key.clone(),
                        request_size_bytes: raw.request_size_bytes,
                        error_message: format!(
                            "failed to normalize graph extraction output after {} provider attempt(s): {}",
                            provider_attempt_count, parse_error,
                        ),
                        provider_failure: Some(provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::InvalidModelOutput,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("terminal_failure".to_string()),
                            !usage_calls.is_empty(),
                        )),
                        recovery_summary: recovery_summary.clone(),
                        recovery_attempts: finalize_recovery_attempt_records(
                            &pending_recovery_records,
                            &recovery_summary,
                        ),
                    });
                }
            }
        }
    }

    Err(GraphExtractionFailureOutcome {
        request_shape_key: "graph_extract_v8:unknown".to_string(),
        request_size_bytes: 0,
        recovery_summary: extraction_recovery.classify_outcome(
            trace.provider_attempt_count,
            pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
            false,
            true,
        ),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
        provider_failure: None,
        recovery_attempts: finalize_recovery_attempt_records(
            &pending_recovery_records,
            &extraction_recovery.classify_outcome(
                trace.provider_attempt_count,
                pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
                false,
                true,
            ),
        ),
    })
}

pub(crate) async fn request_graph_extraction_with_prompt_plan(
    gateway: &dyn LlmGateway,
    _provider_profile: &EffectiveProviderProfile,
    runtime_binding: &ResolvedRuntimeBinding,
    prompt_plan: &GraphExtractionPromptPlan,
    lifecycle: GraphExtractionLifecycle,
) -> Result<RawGraphExtractionResponse> {
    let prompt_hash = graph_extraction_cache_hash(&prompt_plan.prompt, runtime_binding);
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let started_at = Utc::now();
    let started = Instant::now();
    let task_spec = GraphExtractTask::spec();
    let request = build_provider_request(
        &task_spec,
        ChatRequestSeed {
            provider_kind: runtime_binding.provider_kind.clone(),
            model_name: runtime_binding.model_name.clone(),
            api_key_override: runtime_binding.api_key.clone(),
            base_url_override: runtime_binding.provider_base_url.clone(),
            system_prompt: runtime_binding.system_prompt.clone(),
            temperature: runtime_binding.temperature,
            top_p: runtime_binding.top_p,
            max_output_tokens_override: runtime_binding.max_output_tokens_override,
            extra_parameters_json: runtime_binding.extra_parameters_json.clone(),
        },
        prompt_plan.prompt.clone(),
    );
    let response =
        gateway.generate(request).await.context("graph extraction provider call failed")?;
    let finished_at = Utc::now();
    let output_text = response.output_text;
    let usage_json = build_provider_usage_json(&provider_kind, &model_name, response.usage_json);

    Ok(RawGraphExtractionResponse {
        provider_kind,
        model_name,
        prompt_hash,
        request_shape_key: prompt_plan.request_shape_key.clone(),
        request_size_bytes: prompt_plan.request_size_bytes,
        output_text: output_text.clone(),
        usage_json: usage_json.clone(),
        lifecycle,
        timing: build_graph_extraction_call_timing(
            started_at,
            finished_at,
            started.elapsed(),
            &prompt_plan.prompt,
            &output_text,
            &usage_json,
        ),
    })
}

pub(crate) fn build_raw_output_json(
    output_text: &str,
    usage_json: serde_json::Value,
    lifecycle: &GraphExtractionLifecycle,
    recovery: &GraphExtractionRecoveryTrace,
    recovery_summary: &ExtractionRecoverySummary,
    usage_calls: &[GraphExtractionUsageCall],
) -> serde_json::Value {
    serde_json::json!({
        "output_text": output_text,
        "usage": usage_json,
        "provider_calls": usage_calls,
        "lifecycle": lifecycle,
        "recovery": recovery,
        "recovery_summary": recovery_summary,
    })
}

fn build_graph_extraction_call_timing(
    started_at: chrono::DateTime<Utc>,
    finished_at: chrono::DateTime<Utc>,
    elapsed: std::time::Duration,
    prompt: &str,
    output_text: &str,
    usage_json: &serde_json::Value,
) -> GraphExtractionCallTiming {
    let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let input_char_count = i32::try_from(prompt.chars().count()).unwrap_or(i32::MAX);
    let output_char_count = i32::try_from(output_text.chars().count()).unwrap_or(i32::MAX);
    let total_tokens =
        usage_json.get("total_tokens").and_then(serde_json::Value::as_i64).or_else(|| {
            let prompt_tokens =
                usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)?;
            let completion_tokens = usage_json
                .get("completion_tokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Some(prompt_tokens.saturating_add(completion_tokens))
        });
    let seconds = (elapsed_ms > 0).then_some(elapsed_ms as f64 / 1000.0);

    GraphExtractionCallTiming {
        started_at,
        finished_at,
        elapsed_ms,
        input_char_count,
        output_char_count,
        chars_per_second: seconds.and_then(|value| {
            (value > 0.0)
                .then_some(f64::from(input_char_count.saturating_add(output_char_count)) / value)
        }),
        tokens_per_second: seconds.and_then(|value| {
            total_tokens.filter(|tokens| *tokens > 0).map(|tokens| tokens as f64 / value)
        }),
    }
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

pub(crate) fn build_resolved_extraction_from_candidate(
    candidate: ParsedGraphExtractionCandidate,
    provider_kind: &str,
    model_name: &str,
    usage_samples: &[serde_json::Value],
    usage_calls: Vec<GraphExtractionUsageCall>,
    _request_shape_key: String,
    _request_size_bytes: usize,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery: GraphExtractionRecoveryTrace,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> ResolvedGraphExtraction {
    ResolvedGraphExtraction {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        prompt_hash: candidate.raw.prompt_hash.clone(),
        output_text: candidate.raw.output_text.clone(),
        usage_json: aggregate_provider_usage_json(provider_kind, model_name, usage_samples),
        usage_calls,
        provider_failure,
        normalized: candidate.normalized,
        lifecycle: candidate.raw.lifecycle,
        recovery,
        recovery_summary,
        recovery_attempts,
    }
}

pub(crate) fn select_better_partial_candidate(
    existing: Option<ParsedGraphExtractionCandidate>,
    candidate: ParsedGraphExtractionCandidate,
) -> Option<ParsedGraphExtractionCandidate> {
    match existing {
        Some(current)
            if graph_candidate_score(&current.normalized)
                >= graph_candidate_score(&candidate.normalized) =>
        {
            Some(current)
        }
        _ => Some(candidate),
    }
}

fn graph_candidate_score(candidate_set: &GraphExtractionCandidateSet) -> usize {
    candidate_set.entities.len().saturating_mul(2).saturating_add(candidate_set.relations.len())
}

pub(crate) fn finalize_recovery_attempt_records(
    pending_records: &[PendingRecoveryRecord],
    recovery_summary: &ExtractionRecoverySummary,
) -> Vec<GraphExtractionRecoveryRecord> {
    let status = match recovery_summary.status {
        crate::domains::graph_quality::ExtractionOutcomeStatus::Clean => "skipped",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Recovered => "recovered",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Partial => "partial",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Failed => "failed",
    }
    .to_string();

    pending_records
        .iter()
        .map(|record| GraphExtractionRecoveryRecord {
            recovery_kind: record.recovery_kind.clone(),
            trigger_reason: record.trigger_reason.clone(),
            status: status.clone(),
            raw_issue_summary: record.raw_issue_summary.clone(),
            recovered_summary: record.recovered_summary.clone(),
        })
        .collect()
}
