use uuid::Uuid;

use crate::{
    agent_runtime::trace::{RuntimeExecutionTraceView, build_policy_summary, policy_summary},
    app::state::AppState,
    domains::agent_runtime::{RuntimePolicyDecision, RuntimePolicySummary},
    infra::repositories::runtime_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_RUNTIME_READ, load_runtime_execution_and_authorize},
        router_support::{ApiError, map_runtime_execution_row, map_runtime_trace_view},
    },
    mcp_types::{
        McpRuntimeActionSummary, McpRuntimeExecutionSummary, McpRuntimeExecutionTrace,
        McpRuntimePolicySummary, McpRuntimeStageSummary,
    },
};

pub async fn get_runtime_execution(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
) -> Result<McpRuntimeExecutionSummary, ApiError> {
    let row = load_runtime_execution_and_authorize(auth, state, execution_id, POLICY_RUNTIME_READ)
        .await?;
    let policy_rows = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(map_mcp_runtime_execution(
        map_runtime_execution_row(row)?,
        map_runtime_policy_summary(&policy_rows),
    ))
}

pub async fn get_runtime_execution_trace(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
) -> Result<McpRuntimeExecutionTrace, ApiError> {
    let execution_row =
        load_runtime_execution_and_authorize(auth, state, execution_id, POLICY_RUNTIME_READ)
            .await?;
    let stage_rows =
        runtime_repository::list_runtime_stage_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let action_rows =
        runtime_repository::list_runtime_action_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let policy_rows = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(map_mcp_runtime_trace(map_runtime_trace_view(
        execution_row,
        stage_rows,
        action_rows,
        policy_rows,
    )?))
}

fn map_mcp_runtime_execution(
    execution: crate::domains::agent_runtime::RuntimeExecution,
    policy_summary: RuntimePolicySummary,
) -> McpRuntimeExecutionSummary {
    McpRuntimeExecutionSummary {
        runtime_execution_id: execution.id,
        owner_kind: execution.owner_kind,
        owner_id: execution.owner_id,
        task_kind: execution.task_kind,
        surface_kind: execution.surface_kind,
        contract_name: execution.contract_name,
        contract_version: execution.contract_version,
        lifecycle_state: execution.lifecycle_state,
        active_stage: execution.active_stage,
        turn_budget: execution.turn_budget,
        turn_count: execution.turn_count,
        parallel_action_limit: execution.parallel_action_limit,
        failure_code: execution.failure_code,
        failure_summary: execution.failure_summary_redacted,
        policy_summary,
        accepted_at: execution.accepted_at,
        completed_at: execution.completed_at,
    }
}

fn map_mcp_runtime_trace(trace: RuntimeExecutionTraceView) -> McpRuntimeExecutionTrace {
    let execution_policy_summary = policy_summary(&trace);
    McpRuntimeExecutionTrace {
        execution: map_mcp_runtime_execution(trace.execution, execution_policy_summary),
        stages: trace
            .stages
            .into_iter()
            .map(|record| McpRuntimeStageSummary {
                stage_record_id: record.id,
                stage_kind: record.stage_kind,
                stage_ordinal: record.stage_ordinal,
                attempt_no: record.attempt_no,
                stage_state: record.stage_state,
                deterministic: record.deterministic,
                started_at: record.started_at,
                completed_at: record.completed_at,
                failure_code: record.failure_code,
                input_summary: record.input_summary_json,
                output_summary: record.output_summary_json,
            })
            .collect(),
        actions: trace
            .actions
            .into_iter()
            .map(|record| McpRuntimeActionSummary {
                action_id: record.id,
                stage_record_id: record.stage_record_id,
                action_kind: record.action_kind,
                action_ordinal: record.action_ordinal,
                action_state: record.action_state,
                provider_binding_id: record.provider_binding_id,
                tool_name: record.tool_name,
                usage: record.usage_json,
                summary: record.summary_json,
                created_at: record.created_at,
            })
            .collect(),
        policy_decisions: trace
            .policy_decisions
            .into_iter()
            .map(|decision| McpRuntimePolicySummary {
                decision_id: decision.id,
                stage_record_id: decision.stage_record_id,
                action_record_id: decision.action_record_id,
                target_kind: decision.target_kind,
                decision_kind: decision.decision_kind,
                reason_code: decision.reason_code,
                reason_summary: decision.reason_summary_redacted,
                created_at: decision.created_at,
            })
            .collect(),
    }
}

fn map_runtime_policy_summary(
    rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimePolicySummary {
    build_policy_summary(
        &rows
            .iter()
            .map(|row| RuntimePolicyDecision {
                id: row.id,
                runtime_execution_id: row.runtime_execution_id,
                stage_record_id: row.stage_record_id,
                action_record_id: row.action_record_id,
                target_kind: row.target_kind,
                decision_kind: row.decision_kind,
                reason_code: row.reason_code.clone(),
                reason_summary_redacted: row.reason_summary_redacted.clone(),
                created_at: row.created_at,
            })
            .collect::<Vec<_>>(),
    )
}
