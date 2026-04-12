use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    agent_runtime::trace::{RuntimeExecutionTraceView, build_policy_summary, policy_summary},
    app::state::AppState,
    domains::agent_runtime::{
        RuntimeActionRecord, RuntimeExecution, RuntimePolicyDecision, RuntimePolicySummary,
        RuntimeStageRecord,
    },
    infra::repositories::runtime_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_RUNTIME_READ, load_runtime_execution_and_authorize},
        router_support::{
            ApiError, map_runtime_execution_row, map_runtime_policy_decision_row,
            map_runtime_trace_view,
        },
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/runtime/executions/{execution_id}", get(get_runtime_execution))
        .route("/runtime/executions/{execution_id}/trace", get(get_runtime_execution_trace))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeExecutionResponse {
    execution_id: Uuid,
    owner_kind: crate::domains::agent_runtime::RuntimeExecutionOwnerKind,
    owner_id: Uuid,
    task_kind: crate::domains::agent_runtime::RuntimeTaskKind,
    surface_kind: crate::domains::agent_runtime::RuntimeSurfaceKind,
    contract_name: String,
    contract_version: String,
    lifecycle_state: crate::domains::agent_runtime::RuntimeLifecycleState,
    active_stage: Option<crate::domains::agent_runtime::RuntimeStageKind>,
    turn_budget: i32,
    turn_count: i32,
    parallel_action_limit: i32,
    failure_code: Option<String>,
    failure_summary: Option<String>,
    policy_summary: RuntimePolicySummary,
    accepted_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStageRecordResponse {
    stage_record_id: Uuid,
    stage_kind: crate::domains::agent_runtime::RuntimeStageKind,
    stage_ordinal: i32,
    attempt_no: i32,
    stage_state: crate::domains::agent_runtime::RuntimeStageState,
    deterministic: bool,
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_code: Option<String>,
    input_summary: serde_json::Value,
    output_summary: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeActionRecordResponse {
    action_id: Uuid,
    stage_record_id: Uuid,
    action_kind: crate::domains::agent_runtime::RuntimeActionKind,
    action_ordinal: i32,
    action_state: crate::domains::agent_runtime::RuntimeActionState,
    provider_binding_id: Option<Uuid>,
    tool_name: Option<String>,
    usage: Option<serde_json::Value>,
    summary: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimePolicyDecisionResponse {
    decision_id: Uuid,
    stage_record_id: Option<Uuid>,
    action_record_id: Option<Uuid>,
    target_kind: crate::domains::agent_runtime::RuntimeDecisionTargetKind,
    decision_kind: crate::domains::agent_runtime::RuntimeDecisionKind,
    reason_code: String,
    reason_summary: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeExecutionTraceResponse {
    execution: RuntimeExecutionResponse,
    stages: Vec<RuntimeStageRecordResponse>,
    actions: Vec<RuntimeActionRecordResponse>,
    policy_decisions: Vec<RuntimePolicyDecisionResponse>,
}

async fn get_runtime_execution(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<RuntimeExecutionResponse>, ApiError> {
    let execution =
        load_runtime_execution_and_authorize(&auth, &state, execution_id, POLICY_RUNTIME_READ)
            .await?;
    let policy_rows = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(Json(map_runtime_execution_response(
        map_runtime_execution_row(execution)?,
        build_policy_summary(
            &policy_rows
                .into_iter()
                .map(map_runtime_policy_decision_row)
                .collect::<Result<Vec<_>, _>>()?,
        ),
    )))
}

async fn get_runtime_execution_trace(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<RuntimeExecutionTraceResponse>, ApiError> {
    let execution =
        load_runtime_execution_and_authorize(&auth, &state, execution_id, POLICY_RUNTIME_READ)
            .await?;
    let stages =
        runtime_repository::list_runtime_stage_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let actions =
        runtime_repository::list_runtime_action_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let policy_decisions = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let trace = map_runtime_trace_view(execution, stages, actions, policy_decisions)?;
    Ok(Json(map_runtime_trace_response(trace)))
}

fn map_runtime_execution_response(
    execution: RuntimeExecution,
    policy_summary: RuntimePolicySummary,
) -> RuntimeExecutionResponse {
    RuntimeExecutionResponse {
        execution_id: execution.id,
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

fn map_runtime_stage_record_response(record: RuntimeStageRecord) -> RuntimeStageRecordResponse {
    RuntimeStageRecordResponse {
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
    }
}

fn map_runtime_action_record_response(record: RuntimeActionRecord) -> RuntimeActionRecordResponse {
    RuntimeActionRecordResponse {
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
    }
}

fn map_runtime_policy_decision_response(
    decision: RuntimePolicyDecision,
) -> RuntimePolicyDecisionResponse {
    RuntimePolicyDecisionResponse {
        decision_id: decision.id,
        stage_record_id: decision.stage_record_id,
        action_record_id: decision.action_record_id,
        target_kind: decision.target_kind,
        decision_kind: decision.decision_kind,
        reason_code: decision.reason_code,
        reason_summary: decision.reason_summary_redacted,
        created_at: decision.created_at,
    }
}

fn map_runtime_trace_response(trace: RuntimeExecutionTraceView) -> RuntimeExecutionTraceResponse {
    let execution_policy_summary = policy_summary(&trace);
    RuntimeExecutionTraceResponse {
        execution: map_runtime_execution_response(trace.execution, execution_policy_summary),
        stages: trace.stages.into_iter().map(map_runtime_stage_record_response).collect(),
        actions: trace.actions.into_iter().map(map_runtime_action_record_response).collect(),
        policy_decisions: trace
            .policy_decisions
            .into_iter()
            .map(map_runtime_policy_decision_response)
            .collect(),
    }
}
