use anyhow::{Context, Result};
use chrono::Utc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    domains::{
        graph_quality::ExtractionRecoverySummary,
        runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    },
    integrations::llm::{LlmGateway, build_structured_chat_request},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::cancellation::{StageError, anyhow_is_cancelled, ensure_not_cancelled},
        ingest::extraction_recovery::ExtractionRecoveryService,
        ops::provider_failure::{ProviderFailureClassificationService, ProviderFailureObservation},
    },
};

use super::graph_extraction_cache_hash;
use super::parse::{
    normalize_graph_extraction_output, repair_graph_extraction_candidate_set,
    sanitize_graph_extraction_candidate_set,
};
use super::prompt::{
    GRAPH_EXTRACTION_VERSION, build_graph_extraction_prompt_plan, graph_extraction_response_format,
};
use super::types::{
    GraphExtractionCallTiming, GraphExtractionCandidateSet, GraphExtractionFailureOutcome,
    GraphExtractionLifecycle, GraphExtractionPromptPlan, GraphExtractionPromptVariant,
    GraphExtractionRecoveryAttempt, GraphExtractionRecoveryRecord, GraphExtractionRecoveryTrace,
    GraphExtractionRequest, GraphExtractionUsageCall, ParsedGraphExtractionCandidate,
    PendingRecoveryRecord, RawGraphExtractionResponse, RecoveryFollowUpRequest,
    ResolvedGraphExtraction,
};

struct GraphExtractionSessionState {
    trace: GraphExtractionRecoveryTrace,
    usage_samples: Vec<serde_json::Value>,
    usage_calls: Vec<GraphExtractionUsageCall>,
    pending_follow_up: Option<RecoveryFollowUpRequest>,
    pending_recovery_records: Vec<PendingRecoveryRecord>,
    best_partial_candidate: Option<ParsedGraphExtractionCandidate>,
}

impl GraphExtractionSessionState {
    fn new() -> Self {
        Self {
            trace: GraphExtractionRecoveryTrace::default(),
            usage_samples: Vec::new(),
            usage_calls: Vec::new(),
            pending_follow_up: None,
            pending_recovery_records: Vec::new(),
            best_partial_candidate: None,
        }
    }
}

fn recovery_prompt_plan(
    request: &GraphExtractionRequest,
    follow_up: Option<RecoveryFollowUpRequest>,
    request_size_soft_limit_bytes: usize,
) -> GraphExtractionPromptPlan {
    match follow_up {
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
    }
}

pub(crate) async fn resolve_graph_extraction_with_gateway(
    gateway: &dyn LlmGateway,
    extraction_recovery: &ExtractionRecoveryService,
    provider_failure_classification: &ProviderFailureClassificationService,
    runtime_binding: &ResolvedRuntimeBinding,
    request: &GraphExtractionRequest,
    cancellation_token: &CancellationToken,
    recovery_enabled: bool,
    max_provider_attempts: usize,
    provider_timeout_retry_limit: usize,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    if let Some(failure) = ensure_graph_extraction_active(request, cancellation_token) {
        return Err(failure);
    }
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let lifecycle = GraphExtractionLifecycle {
        revision_id: request.revision_id,
        activated_by_attempt_id: request.activated_by_attempt_id,
    };
    let max_provider_attempts = if recovery_enabled { max_provider_attempts.max(1) } else { 1 };
    let recovery_context = GraphExtractionRecoveryContext {
        extraction_recovery,
        provider_failure_classification,
        request,
        provider_kind: &provider_kind,
        model_name: &model_name,
        recovery_enabled,
        max_provider_attempts,
        provider_timeout_retry_limit,
    };
    let mut session = GraphExtractionSessionState::new();
    let request_size_soft_limit_bytes =
        provider_failure_classification.request_size_soft_limit_bytes();

    for provider_attempt_no in 1..=max_provider_attempts {
        if let Some(failure) = ensure_graph_extraction_active(request, cancellation_token) {
            return Err(failure);
        }
        let prompt_plan = recovery_prompt_plan(
            request,
            session.pending_follow_up.take(),
            request_size_soft_limit_bytes,
        );
        let raw = request_graph_extraction_with_prompt_plan(
            gateway,
            runtime_binding,
            &prompt_plan,
            lifecycle.clone(),
            cancellation_token,
        )
        .await;
        let attempt_resolution = match raw {
            Ok(raw) => resolve_raw_graph_extraction_attempt(
                &recovery_context,
                &mut session,
                raw,
                provider_attempt_no,
                cancellation_token,
            ),
            Err(error) => resolve_graph_provider_failure(
                &recovery_context,
                &mut session,
                prompt_plan,
                error,
                provider_attempt_no,
            ),
        };
        if let Some(result) = attempt_resolution {
            return result;
        }
    }

    Err(terminal_graph_extraction_loop_failure(extraction_recovery, &session))
}

type GraphExtractionAttemptResolution =
    Option<std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome>>;

struct GraphExtractionRecoveryContext<'a> {
    extraction_recovery: &'a ExtractionRecoveryService,
    provider_failure_classification: &'a ProviderFailureClassificationService,
    request: &'a GraphExtractionRequest,
    provider_kind: &'a str,
    model_name: &'a str,
    recovery_enabled: bool,
    max_provider_attempts: usize,
    provider_timeout_retry_limit: usize,
}

fn ensure_graph_extraction_active(
    request: &GraphExtractionRequest,
    cancellation_token: &CancellationToken,
) -> Option<GraphExtractionFailureOutcome> {
    cancellation_token.is_cancelled().then(|| {
        cancelled_graph_extraction_failure(
            request,
            format!("{GRAPH_EXTRACTION_VERSION}:cancelled"),
            request.chunk.content.len(),
        )
    })
}

fn resolve_graph_provider_failure(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    prompt_plan: GraphExtractionPromptPlan,
    error: anyhow::Error,
    provider_attempt_no: usize,
) -> GraphExtractionAttemptResolution {
    if anyhow_is_cancelled(&error) {
        return Some(Err(cancelled_graph_extraction_failure(
            context.request,
            prompt_plan.request_shape_key,
            prompt_plan.request_size_bytes,
        )));
    }
    let retry_decision = (provider_attempt_no > 1).then_some("retrying_provider_call");
    let provider_failure = context.provider_failure_classification.classify_failure(
        &error,
        None,
        ProviderFailureObservation {
            provider_kind: Some(context.provider_kind.to_string()),
            model_name: Some(context.model_name.to_string()),
            request_shape_key: Some(prompt_plan.request_shape_key.clone()),
            request_size_bytes: Some(prompt_plan.request_size_bytes),
            chunk_count: Some(1),
            elapsed_ms: None,
            retry_decision: retry_decision.map(str::to_string),
            usage_visible: !session.usage_calls.is_empty(),
        },
    );
    if schedule_transient_graph_provider_retry(
        context,
        session,
        &error,
        provider_failure.as_ref(),
        provider_attempt_no,
    ) {
        return None;
    }
    update_graph_attempt_counts(session, provider_attempt_no);
    if let Some(candidate) = session.best_partial_candidate.clone() {
        return Some(Ok(resolve_partial_graph_candidate(
            context.extraction_recovery,
            session,
            candidate,
            context.provider_kind,
            context.model_name,
            prompt_plan.request_shape_key,
            prompt_plan.request_size_bytes,
            provider_failure,
        )));
    }
    Some(Err(terminal_graph_provider_failure(
        context.extraction_recovery,
        session,
        prompt_plan,
        provider_failure,
        error,
        provider_attempt_no,
    )))
}

fn schedule_transient_graph_provider_retry(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    error: &anyhow::Error,
    provider_failure: Option<&RuntimeProviderFailureDetail>,
    provider_attempt_no: usize,
) -> bool {
    let retry_plan = transient_graph_provider_retry_plan(
        context.provider_failure_classification,
        error,
        provider_failure,
    );
    let can_retry = retry_plan.is_some()
        && provider_attempt_no <= context.provider_timeout_retry_limit
        && provider_attempt_no < context.max_provider_attempts;
    let (true, Some((trigger_reason, recovered_summary))) = (can_retry, retry_plan) else {
        return false;
    };
    let raw_issue_summary =
        context.extraction_recovery.redact_recovery_summary(&format!("{error:#}"));
    session.pending_recovery_records.push(PendingRecoveryRecord {
        recovery_kind: "provider_retry".to_string(),
        trigger_reason: trigger_reason.to_string(),
        raw_issue_summary: Some(raw_issue_summary.clone()),
        recovered_summary: Some(
            context.extraction_recovery.redact_recovery_summary(recovered_summary),
        ),
    });
    session.pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
        trigger_reason: trigger_reason.to_string(),
        issue_summary: raw_issue_summary,
        previous_output: String::new(),
    });
    update_graph_attempt_counts(session, provider_attempt_no);
    true
}

fn transient_graph_provider_retry_plan(
    classification: &ProviderFailureClassificationService,
    error: &anyhow::Error,
    provider_failure: Option<&RuntimeProviderFailureDetail>,
) -> Option<(&'static str, &'static str)> {
    if !classification.is_transient_retryable_error(error) {
        return None;
    }
    match &provider_failure?.failure_class {
        RuntimeProviderFailureClass::UpstreamTimeout => {
            Some(("upstream_timeout", "Retrying graph extraction after an upstream timeout."))
        }
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
}

fn terminal_graph_provider_failure(
    extraction_recovery: &ExtractionRecoveryService,
    session: &GraphExtractionSessionState,
    prompt_plan: GraphExtractionPromptPlan,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    error: anyhow::Error,
    provider_attempt_no: usize,
) -> GraphExtractionFailureOutcome {
    let recovery_summary = terminal_recovery_summary(extraction_recovery, session, false);
    let error_message = if provider_attempt_no == 1 {
        format!("graph extraction provider call failed before normalization retry: {error:#}")
    } else {
        format!("graph extraction recovery attempt {provider_attempt_no} failed: {error:#}")
    };
    GraphExtractionFailureOutcome {
        request_shape_key: prompt_plan.request_shape_key,
        request_size_bytes: prompt_plan.request_size_bytes,
        error_message,
        provider_failure,
        recovery_summary: recovery_summary.clone(),
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}

fn resolve_raw_graph_extraction_attempt(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    raw: RawGraphExtractionResponse,
    provider_attempt_no: usize,
    cancellation_token: &CancellationToken,
) -> GraphExtractionAttemptResolution {
    if cancellation_token.is_cancelled() {
        return Some(Err(cancelled_graph_extraction_failure(
            context.request,
            raw.request_shape_key,
            raw.request_size_bytes,
        )));
    }
    record_graph_extraction_usage(session, &raw, provider_attempt_no);
    match normalize_graph_extraction_output(&raw.output_text) {
        Ok(normalized_attempt) => resolve_normalized_graph_attempt(
            context,
            session,
            raw,
            normalized_attempt,
            provider_attempt_no,
        ),
        Err(parse_failure) => resolve_malformed_graph_attempt(
            context,
            session,
            raw,
            parse_failure,
            provider_attempt_no,
        ),
    }
}

fn record_graph_extraction_usage(
    session: &mut GraphExtractionSessionState,
    raw: &RawGraphExtractionResponse,
    provider_attempt_no: usize,
) {
    session.usage_samples.push(raw.usage_json.clone());
    session.usage_calls.push(GraphExtractionUsageCall {
        provider_call_no: i32::try_from(session.usage_calls.len() + 1).unwrap_or(i32::MAX),
        provider_attempt_no: i32::try_from(provider_attempt_no).unwrap_or(i32::MAX),
        prompt_hash: raw.prompt_hash.clone(),
        request_shape_key: raw.request_shape_key.clone(),
        request_size_bytes: raw.request_size_bytes,
        usage_json: raw.usage_json.clone(),
        timing: raw.timing.clone(),
    });
}

fn resolve_normalized_graph_attempt(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    raw: RawGraphExtractionResponse,
    normalized_attempt: super::types::NormalizedGraphExtractionAttempt,
    provider_attempt_no: usize,
) -> GraphExtractionAttemptResolution {
    update_graph_attempt_counts(session, provider_attempt_no);
    session.trace.attempts.push(GraphExtractionRecoveryAttempt {
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
    let second_pass = context.extraction_recovery.classify_second_pass(
        &context.request.chunk.content,
        normalized_attempt.normalized.entities.len(),
        normalized_attempt.normalized.relations.len(),
        context.recovery_enabled,
        provider_attempt_no,
        context.max_provider_attempts,
    );
    let current_candidate = ParsedGraphExtractionCandidate {
        raw: raw.clone(),
        normalized: sanitize_graph_extraction_candidate_set(
            normalized_attempt.normalized,
            &context.request.chunk.content,
        ),
        normalization_path: normalized_attempt.normalization_path,
    };
    if schedule_graph_second_pass(
        context.extraction_recovery,
        session,
        &raw,
        &current_candidate,
        second_pass,
        provider_attempt_no,
    ) {
        return None;
    }
    Some(Ok(resolve_clean_graph_candidate(
        context,
        session,
        current_candidate,
        &raw,
        provider_attempt_no,
    )))
}

fn schedule_graph_second_pass(
    extraction_recovery: &ExtractionRecoveryService,
    session: &mut GraphExtractionSessionState,
    raw: &RawGraphExtractionResponse,
    candidate: &ParsedGraphExtractionCandidate,
    second_pass: crate::services::ingest::extraction_recovery::SecondPassTrigger,
    provider_attempt_no: usize,
) -> bool {
    if !second_pass.should_attempt {
        return false;
    }
    let decision = second_pass.decision.unwrap_or_else(|| {
        crate::services::ingest::extraction_recovery::RecoveryDecisionSummary {
            reason_code: "sparse_extraction".to_string(),
            reason_summary_redacted: extraction_recovery.redact_recovery_summary(
                "The extraction result looked too sparse for the chunk content.",
            ),
        }
    });
    session.best_partial_candidate =
        select_better_partial_candidate(session.best_partial_candidate.take(), candidate.clone());
    session.pending_recovery_records.push(PendingRecoveryRecord {
        recovery_kind: "second_pass".to_string(),
        trigger_reason: decision.reason_code.clone(),
        raw_issue_summary: Some(decision.reason_summary_redacted.clone()),
        recovered_summary: Some(extraction_recovery.redact_recovery_summary(
            "Requested a second extraction pass because the first result looked sparse or inconsistent.",
        )),
    });
    session.trace.attempts.push(GraphExtractionRecoveryAttempt {
        provider_attempt_no,
        prompt_hash: raw.prompt_hash.clone(),
        output_text: raw.output_text.clone(),
        usage_json: raw.usage_json.clone(),
        timing: raw.timing.clone(),
        parse_error: None,
        normalization_path: candidate.normalization_path.to_string(),
        recovery_kind: Some("second_pass".to_string()),
        trigger_reason: Some(decision.reason_code.clone()),
    });
    session.pending_follow_up = Some(RecoveryFollowUpRequest::SecondPass {
        trigger_reason: decision.reason_code,
        issue_summary: decision.reason_summary_redacted,
        previous_output: raw.output_text.clone(),
    });
    true
}

fn resolve_clean_graph_candidate(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    candidate: ParsedGraphExtractionCandidate,
    raw: &RawGraphExtractionResponse,
    provider_attempt_no: usize,
) -> ResolvedGraphExtraction {
    let recovery_summary = context.extraction_recovery.classify_outcome(
        session.trace.provider_attempt_count,
        session_has_second_pass(session),
        false,
        false,
    );
    let recovery_attempts =
        finalize_recovery_attempt_records(&session.pending_recovery_records, &recovery_summary);
    let provider_failure = (provider_attempt_no > 1).then(|| {
        context.provider_failure_classification.summarize(
            RuntimeProviderFailureClass::RecoveredAfterRetry,
            ProviderFailureObservation {
                provider_kind: Some(raw.provider_kind.clone()),
                model_name: Some(raw.model_name.clone()),
                request_shape_key: Some(raw.request_shape_key.clone()),
                request_size_bytes: Some(raw.request_size_bytes),
                chunk_count: Some(1),
                elapsed_ms: Some(raw.timing.elapsed_ms),
                retry_decision: Some("recovered_after_retry".to_string()),
                usage_visible: true,
            },
        )
    });
    build_terminal_resolved_graph_extraction(
        session,
        candidate,
        &raw.provider_kind,
        &raw.model_name,
        raw.request_shape_key.clone(),
        raw.request_size_bytes,
        provider_failure,
        recovery_summary,
        recovery_attempts,
    )
}

fn resolve_malformed_graph_attempt(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    raw: RawGraphExtractionResponse,
    parse_failure: super::types::FailedNormalizationAttempt,
    provider_attempt_no: usize,
) -> GraphExtractionAttemptResolution {
    let parse_error = parse_failure.parse_error;
    record_malformed_graph_attempt(
        session,
        &raw,
        &parse_error,
        provider_attempt_no,
        context.max_provider_attempts,
    );
    if provider_attempt_no < context.max_provider_attempts {
        schedule_malformed_graph_retry(context.extraction_recovery, session, &raw, &parse_error);
        return None;
    }
    if let Some(candidate) = session.best_partial_candidate.clone() {
        let provider_failure =
            Some(recovered_graph_provider_failure(context.provider_failure_classification, &raw));
        return Some(Ok(resolve_partial_graph_candidate(
            context.extraction_recovery,
            session,
            candidate,
            &raw.provider_kind,
            &raw.model_name,
            raw.request_shape_key.clone(),
            raw.request_size_bytes,
            provider_failure,
        )));
    }
    Some(Err(terminal_malformed_graph_failure(context, session, &raw, parse_error)))
}

fn record_malformed_graph_attempt(
    session: &mut GraphExtractionSessionState,
    raw: &RawGraphExtractionResponse,
    parse_error: &str,
    provider_attempt_no: usize,
    max_provider_attempts: usize,
) {
    session.trace.attempts.push(GraphExtractionRecoveryAttempt {
        provider_attempt_no,
        prompt_hash: raw.prompt_hash.clone(),
        output_text: raw.output_text.clone(),
        usage_json: raw.usage_json.clone(),
        timing: raw.timing.clone(),
        parse_error: Some(parse_error.to_string()),
        normalization_path: "failed".to_string(),
        recovery_kind: (provider_attempt_no < max_provider_attempts)
            .then_some("provider_retry".to_string()),
        trigger_reason: (provider_attempt_no < max_provider_attempts)
            .then_some("malformed_output".to_string()),
    });
    update_graph_attempt_counts(session, provider_attempt_no);
}

fn schedule_malformed_graph_retry(
    extraction_recovery: &ExtractionRecoveryService,
    session: &mut GraphExtractionSessionState,
    raw: &RawGraphExtractionResponse,
    parse_error: &str,
) {
    let parse_error_redacted = extraction_recovery.redact_recovery_summary(parse_error);
    session.pending_recovery_records.push(PendingRecoveryRecord {
        recovery_kind: "provider_retry".to_string(),
        trigger_reason: "malformed_output".to_string(),
        raw_issue_summary: Some(parse_error_redacted.clone()),
        recovered_summary: Some(extraction_recovery.redact_recovery_summary(
            "Requested a stricter retry after malformed extraction output.",
        )),
    });
    session.pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
        trigger_reason: "malformed_output".to_string(),
        issue_summary: parse_error_redacted,
        previous_output: raw.output_text.clone(),
    });
}

fn recovered_graph_provider_failure(
    classification: &ProviderFailureClassificationService,
    raw: &RawGraphExtractionResponse,
) -> RuntimeProviderFailureDetail {
    classification.summarize(
        RuntimeProviderFailureClass::RecoveredAfterRetry,
        ProviderFailureObservation {
            provider_kind: Some(raw.provider_kind.clone()),
            model_name: Some(raw.model_name.clone()),
            request_shape_key: Some(raw.request_shape_key.clone()),
            request_size_bytes: Some(raw.request_size_bytes),
            chunk_count: Some(1),
            elapsed_ms: Some(raw.timing.elapsed_ms),
            retry_decision: Some("recovered_after_retry".to_string()),
            usage_visible: true,
        },
    )
}

fn terminal_malformed_graph_failure(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &GraphExtractionSessionState,
    raw: &RawGraphExtractionResponse,
    parse_error: String,
) -> GraphExtractionFailureOutcome {
    let provider_attempt_count = session.trace.provider_attempt_count;
    let recovery_summary = terminal_recovery_summary(context.extraction_recovery, session, false);
    GraphExtractionFailureOutcome {
        request_shape_key: raw.request_shape_key.clone(),
        request_size_bytes: raw.request_size_bytes,
        error_message: format!(
            "failed to normalize graph extraction output after {provider_attempt_count} provider attempt(s): {parse_error}",
        ),
        provider_failure: Some(context.provider_failure_classification.summarize(
            RuntimeProviderFailureClass::InvalidModelOutput,
            ProviderFailureObservation {
                provider_kind: Some(raw.provider_kind.clone()),
                model_name: Some(raw.model_name.clone()),
                request_shape_key: Some(raw.request_shape_key.clone()),
                request_size_bytes: Some(raw.request_size_bytes),
                chunk_count: Some(1),
                elapsed_ms: Some(raw.timing.elapsed_ms),
                retry_decision: Some("terminal_failure".to_string()),
                usage_visible: !session.usage_calls.is_empty(),
            },
        )),
        recovery_summary: recovery_summary.clone(),
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}

fn resolve_partial_graph_candidate(
    extraction_recovery: &ExtractionRecoveryService,
    session: &mut GraphExtractionSessionState,
    candidate: ParsedGraphExtractionCandidate,
    provider_kind: &str,
    model_name: &str,
    request_shape_key: String,
    request_size_bytes: usize,
    provider_failure: Option<RuntimeProviderFailureDetail>,
) -> ResolvedGraphExtraction {
    let recovery_summary = extraction_recovery.classify_outcome(
        session.trace.provider_attempt_count,
        session_has_second_pass(session),
        true,
        false,
    );
    let recovery_attempts =
        finalize_recovery_attempt_records(&session.pending_recovery_records, &recovery_summary);
    build_terminal_resolved_graph_extraction(
        session,
        candidate,
        provider_kind,
        model_name,
        request_shape_key,
        request_size_bytes,
        provider_failure,
        recovery_summary,
        recovery_attempts,
    )
}

fn build_terminal_resolved_graph_extraction(
    session: &mut GraphExtractionSessionState,
    candidate: ParsedGraphExtractionCandidate,
    provider_kind: &str,
    model_name: &str,
    request_shape_key: String,
    request_size_bytes: usize,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> ResolvedGraphExtraction {
    build_resolved_extraction_from_candidate(
        candidate,
        provider_kind,
        model_name,
        &session.usage_samples,
        std::mem::take(&mut session.usage_calls),
        request_shape_key,
        request_size_bytes,
        provider_failure,
        std::mem::take(&mut session.trace),
        recovery_summary,
        recovery_attempts,
    )
}

fn update_graph_attempt_counts(
    session: &mut GraphExtractionSessionState,
    provider_attempt_no: usize,
) {
    session.trace.provider_attempt_count = provider_attempt_no;
    session.trace.reask_count = provider_attempt_no.saturating_sub(1);
}

fn session_has_second_pass(session: &GraphExtractionSessionState) -> bool {
    session.pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass")
}

fn terminal_recovery_summary(
    extraction_recovery: &ExtractionRecoveryService,
    session: &GraphExtractionSessionState,
    used_partial_candidate: bool,
) -> ExtractionRecoverySummary {
    extraction_recovery.classify_outcome(
        session.trace.provider_attempt_count,
        session_has_second_pass(session),
        used_partial_candidate,
        !used_partial_candidate,
    )
}

fn terminal_graph_extraction_loop_failure(
    extraction_recovery: &ExtractionRecoveryService,
    session: &GraphExtractionSessionState,
) -> GraphExtractionFailureOutcome {
    let recovery_summary = terminal_recovery_summary(extraction_recovery, session, false);
    GraphExtractionFailureOutcome {
        request_shape_key: format!("{GRAPH_EXTRACTION_VERSION}:unknown"),
        request_size_bytes: 0,
        recovery_summary: recovery_summary.clone(),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
        provider_failure: None,
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}

pub(crate) async fn request_graph_extraction_with_prompt_plan(
    gateway: &dyn LlmGateway,
    runtime_binding: &ResolvedRuntimeBinding,
    prompt_plan: &GraphExtractionPromptPlan,
    lifecycle: GraphExtractionLifecycle,
    cancellation_token: &CancellationToken,
) -> Result<RawGraphExtractionResponse> {
    ensure_not_cancelled(cancellation_token)?;
    let prompt_hash = graph_extraction_cache_hash(&prompt_plan.prompt, runtime_binding);
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let started_at = Utc::now();
    let started = Instant::now();
    let request = build_structured_chat_request(
        runtime_binding.chat_request_seed(),
        prompt_plan.prompt.clone(),
        graph_extraction_response_format(),
    );
    let response = tokio::select! {
        () = cancellation_token.cancelled() => {
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = gateway.generate(request) => result.context("graph extraction provider call failed")?,
    };
    ensure_not_cancelled(cancellation_token)?;
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

fn cancelled_graph_extraction_failure(
    _request: &GraphExtractionRequest,
    request_shape_key: impl Into<String>,
    request_size_bytes: usize,
) -> GraphExtractionFailureOutcome {
    GraphExtractionFailureOutcome {
        request_shape_key: request_shape_key.into(),
        request_size_bytes,
        error_message: StageError::Cancelled.to_string(),
        provider_failure: None,
        recovery_summary: ExtractionRecoverySummary {
            status: crate::domains::graph_quality::ExtractionOutcomeStatus::Failed,
            second_pass_applied: false,
            warning: None,
        },
        recovery_attempts: Vec::new(),
        cancelled: true,
    }
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
    let normalized = repair_graph_extraction_candidate_set(candidate.normalized);
    if super::parse::graph_extraction_candidate_set_contains_encoding_damage(&normalized) {
        tracing::error!(
            prompt_hash = %candidate.raw.prompt_hash,
            "graph extraction candidate retained encoding damage after repair"
        );
    }

    ResolvedGraphExtraction {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        prompt_hash: candidate.raw.prompt_hash.clone(),
        output_text: candidate.raw.output_text.clone(),
        usage_json: aggregate_provider_usage_json(provider_kind, model_name, usage_samples),
        usage_calls,
        provider_failure,
        normalized,
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

const fn graph_candidate_score(candidate_set: &GraphExtractionCandidateSet) -> usize {
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
