use tokio_util::sync::CancellationToken;

use crate::{
    domains::graph_quality::ExtractionRecoverySummary,
    integrations::llm::LlmGateway,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        graph::extract::prompt::{GRAPH_EXTRACTION_VERSION, build_graph_extraction_prompt_plan},
        graph::extract::types::{
            GraphExtractionFailureOutcome, GraphExtractionLifecycle, GraphExtractionPromptPlan,
            GraphExtractionPromptVariant, GraphExtractionRecoveryRecord,
            GraphExtractionRecoveryTrace, GraphExtractionRequest, GraphExtractionUsageCall,
            ParsedGraphExtractionCandidate, PendingRecoveryRecord, RecoveryFollowUpRequest,
            ResolvedGraphExtraction,
        },
        ingest::cancellation::StageError,
        ingest::extraction_recovery::ExtractionRecoveryService,
        ops::provider_failure::ProviderFailureClassificationService,
    },
};

use super::attempt_outcome::resolve_raw_graph_extraction_attempt;
use super::provider_call::request_graph_extraction_with_prompt_plan;
use super::provider_failure::resolve_graph_provider_failure;
use super::resolved_extraction::{
    GraphExtractionProviderRecord, build_resolved_extraction_from_candidate,
    finalize_recovery_attempt_records,
};

pub(super) struct GraphExtractionSessionState {
    pub(super) trace: GraphExtractionRecoveryTrace,
    pub(super) usage_samples: Vec<serde_json::Value>,
    pub(super) usage_calls: Vec<GraphExtractionUsageCall>,
    pub(super) pending_follow_up: Option<RecoveryFollowUpRequest>,
    pub(super) pending_recovery_records: Vec<PendingRecoveryRecord>,
    pub(super) best_partial_candidate: Option<ParsedGraphExtractionCandidate>,
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

/// Caller-supplied inputs of one graph extraction retry loop: the recovery and
/// failure services, the resolved binding, the request, and the recovery
/// budget. Grouping them keeps [`resolve_graph_extraction_with_gateway`] and
/// the per-attempt [`GraphExtractionRecoveryContext`] from drifting apart.
pub(crate) struct GraphExtractionLoopRequest<'a> {
    pub(crate) extraction_recovery: &'a ExtractionRecoveryService,
    pub(crate) provider_failure_classification: &'a ProviderFailureClassificationService,
    pub(crate) runtime_binding: &'a ResolvedRuntimeBinding,
    pub(crate) request: &'a GraphExtractionRequest,
    pub(crate) recovery_enabled: bool,
    pub(crate) max_provider_attempts: usize,
    pub(crate) provider_timeout_retry_limit: usize,
}

pub(crate) async fn resolve_graph_extraction_with_gateway(
    gateway: &dyn LlmGateway,
    loop_request: GraphExtractionLoopRequest<'_>,
    cancellation_token: &CancellationToken,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let GraphExtractionLoopRequest {
        extraction_recovery,
        provider_failure_classification,
        runtime_binding,
        request,
        recovery_enabled,
        max_provider_attempts,
        provider_timeout_retry_limit,
    } = loop_request;
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

pub(super) type GraphExtractionAttemptResolution =
    Option<std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome>>;

pub(super) struct GraphExtractionRecoveryContext<'a> {
    pub(super) extraction_recovery: &'a ExtractionRecoveryService,
    pub(super) provider_failure_classification: &'a ProviderFailureClassificationService,
    pub(super) request: &'a GraphExtractionRequest,
    pub(super) provider_kind: &'a str,
    pub(super) model_name: &'a str,
    pub(super) recovery_enabled: bool,
    pub(super) max_provider_attempts: usize,
    pub(super) provider_timeout_retry_limit: usize,
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

pub(super) fn resolve_partial_graph_candidate(
    extraction_recovery: &ExtractionRecoveryService,
    session: &mut GraphExtractionSessionState,
    candidate: ParsedGraphExtractionCandidate,
    provider: GraphExtractionProviderRecord<'_>,
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
        provider,
        recovery_summary,
        recovery_attempts,
    )
}

pub(super) fn build_terminal_resolved_graph_extraction(
    session: &mut GraphExtractionSessionState,
    candidate: ParsedGraphExtractionCandidate,
    provider: GraphExtractionProviderRecord<'_>,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> ResolvedGraphExtraction {
    build_resolved_extraction_from_candidate(
        candidate,
        provider,
        session,
        recovery_summary,
        recovery_attempts,
    )
}

pub(super) fn update_graph_attempt_counts(
    session: &mut GraphExtractionSessionState,
    provider_attempt_no: usize,
) {
    session.trace.provider_attempt_count = provider_attempt_no;
    session.trace.reask_count = provider_attempt_no.saturating_sub(1);
}

pub(super) fn session_has_second_pass(session: &GraphExtractionSessionState) -> bool {
    session.pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass")
}

pub(super) fn terminal_recovery_summary(
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
        recovery_summary: Box::new(recovery_summary.clone()),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
        provider_failure: None,
        provider_admission_error: None,
        recovery_attempts: finalize_recovery_attempt_records(
            &session.pending_recovery_records,
            &recovery_summary,
        ),
        cancelled: false,
    }
}

pub(super) fn cancelled_graph_extraction_failure(
    _request: &GraphExtractionRequest,
    request_shape_key: impl Into<String>,
    request_size_bytes: usize,
) -> GraphExtractionFailureOutcome {
    GraphExtractionFailureOutcome {
        request_shape_key: request_shape_key.into(),
        request_size_bytes,
        error_message: StageError::Cancelled.to_string(),
        provider_failure: None,
        provider_admission_error: None,
        recovery_summary: Box::new(ExtractionRecoverySummary {
            status: crate::domains::graph_quality::ExtractionOutcomeStatus::Failed,
            second_pass_applied: false,
            warning: None,
        }),
        recovery_attempts: Vec::new(),
        cancelled: true,
    }
}
