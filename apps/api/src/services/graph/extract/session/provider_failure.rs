use crate::{
    domains::runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError},
    services::{
        graph::extract::types::{
            GraphExtractionFailureOutcome, GraphExtractionPromptPlan, PendingRecoveryRecord,
            RawGraphExtractionResponse, RecoveryFollowUpRequest,
        },
        ingest::cancellation::anyhow_is_cancelled,
        ingest::extraction_recovery::ExtractionRecoveryService,
        ops::provider_failure::{ProviderFailureClassificationService, ProviderFailureObservation},
    },
};

use super::recovery_loop::{
    GraphExtractionAttemptResolution, GraphExtractionRecoveryContext, GraphExtractionSessionState,
    cancelled_graph_extraction_failure, resolve_partial_graph_candidate, terminal_recovery_summary,
    update_graph_attempt_counts,
};
use super::resolved_extraction::{
    GraphExtractionProviderRecord, finalize_recovery_attempt_records,
};

pub(super) fn resolve_graph_provider_failure(
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
    let provider_admission_failed = provider_admission_error(&error).is_some();
    if !provider_admission_failed
        && schedule_transient_graph_provider_retry(
            context,
            session,
            &error,
            provider_failure.as_ref(),
            provider_attempt_no,
        )
    {
        return None;
    }
    update_graph_attempt_counts(session, provider_attempt_no);
    if !provider_admission_failed && let Some(candidate) = session.best_partial_candidate.clone() {
        return Some(Ok(resolve_partial_graph_candidate(
            context.extraction_recovery,
            session,
            candidate,
            GraphExtractionProviderRecord {
                provider_kind: context.provider_kind,
                model_name: context.model_name,
                provider_failure,
            },
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
    let provider_admission_error = provider_admission_error(&error).cloned();
    let error_message = if provider_attempt_no == 1 {
        format!("graph extraction provider call failed before normalization retry: {error:#}")
    } else {
        format!("graph extraction recovery attempt {provider_attempt_no} failed: {error:#}")
    };
    GraphExtractionFailureOutcome {
        request_shape_key: prompt_plan.request_shape_key,
        request_size_bytes: prompt_plan.request_size_bytes,
        error_message,
        provider_failure: provider_failure.map(Box::new),
        provider_admission_error: provider_admission_error.map(Box::new),
        recovery_summary: Box::new(recovery_summary.clone()),
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}

fn provider_admission_error(error: &anyhow::Error) -> Option<&ProviderBudgetError> {
    error.downcast_ref::<ProviderCallError>().and_then(ProviderCallError::admission_error)
}

pub(super) fn recovered_graph_provider_failure(
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
