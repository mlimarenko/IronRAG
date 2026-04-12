use crate::{
    agent_runtime::executor::{RuntimeExecutionError, RuntimeExecutionSession},
    agent_runtime::response::{RuntimeFailureSummary, RuntimeTerminalOutcome},
    domains::{
        agent_runtime::{RuntimeDecisionKind, RuntimeStageKind, RuntimeStageState},
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
        runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    },
    infra::repositories::RuntimeGraphExtractionRecordRow,
};

use super::prompt::normalized_downgrade_level;
use super::types::*;

pub(crate) fn unconfigured_graph_extraction_failure(
    _request: &GraphExtractionRequest,
    error_message: impl Into<String>,
) -> GraphExtractionFailureOutcome {
    GraphExtractionFailureOutcome {
        request_shape_key: "graph_extract_v6:unconfigured".to_string(),
        request_size_bytes: 0,
        error_message: error_message.into(),
        provider_failure: None,
        recovery_summary: ExtractionRecoverySummary {
            status: ExtractionOutcomeStatus::Failed,
            second_pass_applied: false,
            warning: None,
        },
        recovery_attempts: Vec::new(),
    }
}

pub(crate) fn map_graph_runtime_execution_error(
    request: &GraphExtractionRequest,
    _runtime_execution_id: Option<uuid::Uuid>,
    error: RuntimeExecutionError,
) -> GraphExtractionExecutionError {
    let message = match error {
        RuntimeExecutionError::InvalidTaskSpec(message) => message,
        RuntimeExecutionError::UnregisteredTask(task_kind) => {
            format!("runtime task is not registered: {}", task_kind.as_str())
        }
        RuntimeExecutionError::TurnBudgetExhausted => {
            "runtime execution budget exhausted".to_string()
        }
        RuntimeExecutionError::PolicyBlocked { reason_code, reason_summary_redacted, .. } => {
            format!("{reason_code}: {reason_summary_redacted}")
        }
    };
    graph_extraction_execution_error(
        request,
        message,
        None,
        ExtractionRecoverySummary {
            status: ExtractionOutcomeStatus::Failed,
            second_pass_applied: false,
            warning: None,
        },
        Vec::new(),
    )
}

pub(crate) fn graph_extraction_execution_error(
    request: &GraphExtractionRequest,
    message: impl Into<String>,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> GraphExtractionExecutionError {
    GraphExtractionExecutionError {
        message: message.into(),
        request_shape_key: "graph_extract_v6:runtime".to_string(),
        request_size_bytes: request.chunk.content.len(),
        provider_failure,
        recovery_summary,
        recovery_attempts,
        resume_state: GraphExtractionResumeState {
            resumed_from_checkpoint: false,
            replay_count: request.resume_hint.as_ref().map(|hint| hint.replay_count).unwrap_or(0),
            downgrade_level: normalized_downgrade_level(request),
        },
    }
}

pub(crate) fn make_graph_terminal_failure_outcome(
    failure: GraphExtractionTaskFailure,
) -> RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure> {
    let summary = make_graph_runtime_failure_summary(&failure.code, &failure.summary);
    if matches!(
        failure.code.as_str(),
        "runtime_policy_rejected" | "runtime_policy_terminated" | "runtime_policy_blocked"
    ) {
        RuntimeTerminalOutcome::Canceled { failure, summary }
    } else {
        RuntimeTerminalOutcome::Failed { failure, summary }
    }
}

pub(crate) fn graph_async_operation_status(
    outcome: &RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure>,
) -> &'static str {
    match outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            "ready"
        }
        RuntimeTerminalOutcome::Canceled { .. } => "canceled",
        RuntimeTerminalOutcome::Failed { .. } => "failed",
    }
}

pub(crate) fn graph_failure_code_from_outcome(
    failure: &GraphExtractionFailureOutcome,
) -> &'static str {
    match failure.provider_failure.as_ref().map(|value| value.failure_class.clone()) {
        Some(RuntimeProviderFailureClass::InvalidModelOutput) => {
            GraphExtractionTaskFailureCode::MalformedOutput.as_str()
        }
        _ => "graph_extract_failed",
    }
}

pub(crate) fn make_graph_runtime_failure_summary(
    code: &str,
    summary: &str,
) -> RuntimeFailureSummary {
    RuntimeFailureSummary {
        code: code.to_string(),
        summary_redacted: Some(truncate_failure_code(summary).to_string()),
    }
}

pub(crate) fn truncate_failure_code(message: &str) -> &str {
    const MAX_LEN: usize = 160;
    if message.len() <= MAX_LEN {
        return message;
    }
    let mut end = MAX_LEN;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

fn graph_policy_action_kind(failure_code: &str) -> Option<&'static str> {
    match failure_code {
        "runtime_policy_rejected" => Some("graph_extract.runtime.policy.rejected"),
        "runtime_policy_terminated" => Some("graph_extract.runtime.policy.terminated"),
        "runtime_policy_blocked" => Some("graph_extract.runtime.policy.blocked"),
        _ => None,
    }
}

pub(crate) async fn append_graph_runtime_policy_audit(
    state: &crate::app::state::AppState,
    request: &GraphExtractionRequest,
    graph_extraction_id: uuid::Uuid,
    runtime_result: &crate::agent_runtime::task::RuntimeTaskResult<
        crate::agent_runtime::tasks::graph_extract::GraphExtractTask,
    >,
) {
    let RuntimeTerminalOutcome::Canceled { summary, .. } = &runtime_result.outcome else {
        return;
    };
    let Some(action_kind) = graph_policy_action_kind(&summary.code) else {
        return;
    };
    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            state,
            crate::services::iam::audit::AppendAuditEventCommand {
                actor_principal_id: None,
                surface_kind: "worker".to_string(),
                action_kind: action_kind.to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "rejected".to_string(),
                redacted_message: summary.summary_redacted.clone(),
                internal_message: Some(format!(
                    "runtime policy canceled graph extraction {} for document {} via runtime execution {} with code {}",
                    graph_extraction_id, request.document.id, runtime_result.execution.id, summary.code
                )),
                subjects: vec![state.canonical_services.audit.runtime_execution_subject(
                    runtime_result.execution.id,
                    None,
                    None,
                )],
            },
        )
        .await
    {
        tracing::warn!(stage = "graph", error = %error, "audit append failed");
    }
}

pub(crate) async fn begin_graph_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
) -> std::result::Result<(), GraphExtractionTaskFailure> {
    executor.begin_stage(session, stage_kind).await.map_err(|error| match error {
        RuntimeExecutionError::TurnBudgetExhausted => GraphExtractionTaskFailure {
            code: "runtime_budget_exhausted".to_string(),
            summary: "runtime execution budget exhausted".to_string(),
        },
        RuntimeExecutionError::InvalidTaskSpec(message) => GraphExtractionTaskFailure {
            code: "invalid_runtime_task_spec".to_string(),
            summary: message,
        },
        RuntimeExecutionError::UnregisteredTask(task_kind) => GraphExtractionTaskFailure {
            code: "unregistered_runtime_task".to_string(),
            summary: format!("runtime task is not registered: {}", task_kind.as_str()),
        },
        RuntimeExecutionError::PolicyBlocked {
            decision_kind,
            reason_code,
            reason_summary_redacted,
        } => GraphExtractionTaskFailure {
            code: match decision_kind {
                RuntimeDecisionKind::Reject => "runtime_policy_rejected".to_string(),
                RuntimeDecisionKind::Terminate => "runtime_policy_terminated".to_string(),
                RuntimeDecisionKind::Allow => "runtime_policy_blocked".to_string(),
            },
            summary: format!("{reason_code}: {reason_summary_redacted}"),
        },
    })
}

pub(crate) fn record_graph_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
    stage_state: RuntimeStageState,
    deterministic: bool,
    failure: Option<&GraphExtractionTaskFailure>,
) {
    executor.complete_stage(
        session,
        stage_kind,
        stage_state,
        deterministic,
        failure.map(|value| value.code.clone()),
        failure.map(|value| truncate_failure_code(&value.summary).to_string()),
    );
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

#[must_use]
pub fn extraction_recovery_summary_from_record(
    record: &RuntimeGraphExtractionRecordRow,
) -> Option<ExtractionRecoverySummary> {
    record
        .raw_output_json
        .get("recovery_summary")
        .and_then(|value| serde_json::from_value::<ExtractionRecoverySummary>(value.clone()).ok())
}
