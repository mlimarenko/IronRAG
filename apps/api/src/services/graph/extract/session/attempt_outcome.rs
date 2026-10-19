use tokio_util::sync::CancellationToken;

use crate::{
    domains::runtime_ingestion::RuntimeProviderFailureClass,
    services::{
        graph::extract::parse::{
            normalize_graph_extraction_output, sanitize_graph_extraction_candidate_set,
        },
        graph::extract::types::{
            FailedNormalizationAttempt, GraphExtractionFailureOutcome,
            GraphExtractionRecoveryAttempt, GraphExtractionUsageCall,
            NormalizedGraphExtractionAttempt, ParsedGraphExtractionCandidate,
            PendingRecoveryRecord, RawGraphExtractionResponse, RecoveryFollowUpRequest,
            ResolvedGraphExtraction,
        },
        ingest::extraction_recovery::{ExtractionRecoveryService, SecondPassPolicy},
        ops::provider_failure::ProviderFailureObservation,
    },
};

use super::provider_failure::recovered_graph_provider_failure;
use super::recovery_loop::{
    GraphExtractionAttemptResolution, GraphExtractionRecoveryContext, GraphExtractionSessionState,
    build_terminal_resolved_graph_extraction, cancelled_graph_extraction_failure,
    resolve_partial_graph_candidate, session_has_second_pass, terminal_recovery_summary,
    update_graph_attempt_counts,
};
use super::resolved_extraction::{
    GraphExtractionProviderRecord, finalize_recovery_attempt_records,
    select_better_partial_candidate,
};

pub(super) fn resolve_raw_graph_extraction_attempt(
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
    normalized_attempt: NormalizedGraphExtractionAttempt,
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
        SecondPassPolicy {
            enabled: context.recovery_enabled,
            attempt_no: provider_attempt_no,
            max_attempts: context.max_provider_attempts,
        },
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
        GraphExtractionProviderRecord {
            provider_kind: &raw.provider_kind,
            model_name: &raw.model_name,
            provider_failure,
        },
        recovery_summary,
        recovery_attempts,
    )
}

fn resolve_malformed_graph_attempt(
    context: &GraphExtractionRecoveryContext<'_>,
    session: &mut GraphExtractionSessionState,
    raw: RawGraphExtractionResponse,
    parse_failure: FailedNormalizationAttempt,
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
            GraphExtractionProviderRecord {
                provider_kind: &raw.provider_kind,
                model_name: &raw.model_name,
                provider_failure,
            },
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
        provider_failure: Some(Box::new(context.provider_failure_classification.summarize(
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
        ))),
        provider_admission_error: None,
        recovery_summary: Box::new(recovery_summary.clone()),
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}
