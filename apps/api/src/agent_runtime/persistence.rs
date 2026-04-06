use sqlx::PgPool;

use crate::{
    agent_runtime::trace::RuntimeExecutionTraceView,
    domains::agent_runtime::{RuntimeExecution, RuntimeStageKind},
    infra::repositories::runtime_repository,
};

/// # Errors
/// Returns any `SQLx` error raised while inserting the canonical runtime execution row.
pub async fn create_runtime_execution(
    pool: &PgPool,
    execution: &RuntimeExecution,
) -> Result<runtime_repository::RuntimeExecutionRow, sqlx::Error> {
    runtime_repository::create_runtime_execution(
        pool,
        &runtime_repository::NewRuntimeExecution {
            id: execution.id,
            owner_kind: execution.owner_kind.as_str(),
            owner_id: execution.owner_id,
            task_kind: execution.task_kind.as_str(),
            surface_kind: execution.surface_kind.as_str(),
            contract_name: &execution.contract_name,
            contract_version: &execution.contract_version,
            lifecycle_state: execution.lifecycle_state.as_str(),
            active_stage: execution.active_stage.map(RuntimeStageKind::as_str),
            turn_budget: execution.turn_budget,
            turn_count: execution.turn_count,
            parallel_action_limit: execution.parallel_action_limit,
            failure_code: execution.failure_code.as_deref(),
            failure_summary_redacted: execution.failure_summary_redacted.as_deref(),
        },
    )
    .await
}

/// # Errors
/// Returns any `SQLx` error raised while updating the runtime execution row or persisting its trace.
pub async fn persist_runtime_result(
    pool: &PgPool,
    execution: &RuntimeExecution,
    trace: &RuntimeExecutionTraceView,
) -> Result<(), sqlx::Error> {
    runtime_repository::update_runtime_execution(
        pool,
        execution.id,
        &runtime_repository::UpdateRuntimeExecution {
            lifecycle_state: execution.lifecycle_state.as_str(),
            active_stage: execution.active_stage.map(RuntimeStageKind::as_str),
            turn_count: execution.turn_count,
            failure_code: execution.failure_code.as_deref(),
            failure_summary_redacted: execution.failure_summary_redacted.as_deref(),
            completed_at: execution.completed_at,
        },
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;

    for stage in &trace.stages {
        runtime_repository::create_runtime_stage_record(
            pool,
            &runtime_repository::NewRuntimeStageRecord {
                id: stage.id,
                runtime_execution_id: stage.runtime_execution_id,
                stage_kind: stage.stage_kind.as_str(),
                stage_ordinal: stage.stage_ordinal,
                attempt_no: stage.attempt_no,
                stage_state: stage.stage_state.as_str(),
                deterministic: stage.deterministic,
                input_summary_json: stage.input_summary_json.clone(),
                output_summary_json: stage.output_summary_json.clone(),
                failure_code: stage.failure_code.as_deref(),
                failure_summary_redacted: stage.failure_summary_redacted.as_deref(),
            },
        )
        .await?;
    }

    for action in &trace.actions {
        runtime_repository::create_runtime_action_record(
            pool,
            &runtime_repository::NewRuntimeActionRecord {
                id: action.id,
                runtime_execution_id: action.runtime_execution_id,
                stage_record_id: action.stage_record_id,
                action_kind: action.action_kind.as_str(),
                action_ordinal: action.action_ordinal,
                action_state: action.action_state.as_str(),
                provider_binding_id: action.provider_binding_id,
                tool_name: action.tool_name.as_deref(),
                usage_json: action.usage_json.clone(),
                summary_json: action.summary_json.clone(),
            },
        )
        .await?;
    }

    for decision in &trace.policy_decisions {
        runtime_repository::create_runtime_policy_decision(
            pool,
            &runtime_repository::NewRuntimePolicyDecision {
                id: decision.id,
                runtime_execution_id: decision.runtime_execution_id,
                stage_record_id: decision.stage_record_id,
                action_record_id: decision.action_record_id,
                target_kind: decision.target_kind.as_str(),
                decision_kind: decision.decision_kind.as_str(),
                reason_code: &decision.reason_code,
                reason_summary_redacted: &decision.reason_summary_redacted,
            },
        )
        .await?;
    }

    Ok(())
}
