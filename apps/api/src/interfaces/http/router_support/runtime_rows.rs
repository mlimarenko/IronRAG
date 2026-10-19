use super::api_error::ApiError;
use crate::{
    agent_runtime::trace::{RuntimeExecutionTraceView, build_trace_view},
    domains::agent_runtime::{
        RuntimeActionRecord, RuntimeExecution, RuntimePolicyDecision, RuntimeStageRecord,
    },
    infra::repositories::runtime_repository,
};

pub fn map_runtime_execution_row(
    row: runtime_repository::RuntimeExecutionRow,
) -> Result<RuntimeExecution, ApiError> {
    Ok(RuntimeExecution {
        id: row.id,
        owner_kind: row.owner_kind,
        owner_id: row.owner_id,
        task_kind: row.task_kind,
        surface_kind: row.surface_kind,
        contract_name: row.contract_name,
        contract_version: row.contract_version,
        lifecycle_state: row.lifecycle_state,
        active_stage: row.active_stage,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
        accepted_at: row.accepted_at,
        completed_at: row.completed_at,
    })
}

pub fn map_runtime_stage_record_row(
    row: runtime_repository::RuntimeStageRecordRow,
) -> Result<RuntimeStageRecord, ApiError> {
    Ok(RuntimeStageRecord {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_kind: row.stage_kind,
        stage_ordinal: row.stage_ordinal,
        attempt_no: row.attempt_no,
        stage_state: row.stage_state,
        deterministic: row.deterministic,
        started_at: row.started_at,
        completed_at: row.completed_at,
        input_summary_json: row.input_summary_json,
        output_summary_json: row.output_summary_json,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
    })
}

pub fn map_runtime_action_record_row(
    row: runtime_repository::RuntimeActionRecordRow,
) -> Result<RuntimeActionRecord, ApiError> {
    Ok(RuntimeActionRecord {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_kind: row.action_kind,
        action_ordinal: row.action_ordinal,
        action_state: row.action_state,
        provider_binding_id: row.provider_binding_id,
        tool_name: row.tool_name,
        usage_json: row.usage_json,
        summary_json: row.summary_json,
        created_at: row.created_at,
    })
}

pub fn map_runtime_policy_decision_row(
    row: runtime_repository::RuntimePolicyDecisionRow,
) -> Result<RuntimePolicyDecision, ApiError> {
    Ok(RuntimePolicyDecision {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_record_id: row.action_record_id,
        target_kind: row.target_kind,
        decision_kind: row.decision_kind,
        reason_code: row.reason_code,
        reason_summary_redacted: row.reason_summary_redacted,
        created_at: row.created_at,
    })
}

pub fn map_runtime_trace_view(
    execution: runtime_repository::RuntimeExecutionRow,
    stages: Vec<runtime_repository::RuntimeStageRecordRow>,
    actions: Vec<runtime_repository::RuntimeActionRecordRow>,
    policy_decisions: Vec<runtime_repository::RuntimePolicyDecisionRow>,
) -> Result<RuntimeExecutionTraceView, ApiError> {
    Ok(build_trace_view(
        map_runtime_execution_row(execution)?,
        stages.into_iter().map(map_runtime_stage_record_row).collect::<Result<Vec<_>, _>>()?,
        actions.into_iter().map(map_runtime_action_record_row).collect::<Result<Vec<_>, _>>()?,
        policy_decisions
            .into_iter()
            .map(map_runtime_policy_decision_row)
            .collect::<Result<Vec<_>, _>>()?,
    ))
}
