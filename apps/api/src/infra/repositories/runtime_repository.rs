#![allow(clippy::missing_const_for_fn, clippy::missing_errors_doc)]

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domains::agent_runtime::{
    RuntimeActionKind, RuntimeActionState, RuntimeDecisionKind, RuntimeDecisionTargetKind,
    RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeStageKind, RuntimeStageState,
    RuntimeSurfaceKind, RuntimeTaskKind,
};

#[derive(Debug, Clone, FromRow)]
struct RuntimeExecutionRowRecord {
    id: Uuid,
    owner_kind_key: String,
    owner_id: Uuid,
    task_kind_key: String,
    surface_kind_key: String,
    contract_name: String,
    contract_version: String,
    lifecycle_state_key: String,
    active_stage_key: Option<String>,
    turn_budget: i32,
    turn_count: i32,
    parallel_action_limit: i32,
    failure_code: Option<String>,
    failure_summary_redacted: Option<String>,
    accepted_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct RuntimeExecutionRow {
    pub id: Uuid,
    pub owner_kind: RuntimeExecutionOwnerKind,
    pub owner_id: Uuid,
    pub task_kind: RuntimeTaskKind,
    pub surface_kind: RuntimeSurfaceKind,
    pub contract_name: String,
    pub contract_version: String,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct RuntimeStageRecordRowRecord {
    id: Uuid,
    runtime_execution_id: Uuid,
    stage_kind_key: String,
    stage_ordinal: i32,
    attempt_no: i32,
    stage_state_key: String,
    deterministic: bool,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    input_summary_json: serde_json::Value,
    output_summary_json: serde_json::Value,
    failure_code: Option<String>,
    failure_summary_redacted: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct RuntimeStageRecordRow {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_kind: RuntimeStageKind,
    pub stage_ordinal: i32,
    pub attempt_no: i32,
    pub stage_state: RuntimeStageState,
    pub deterministic: bool,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub input_summary_json: serde_json::Value,
    pub output_summary_json: serde_json::Value,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
struct RuntimeActionRecordRowRecord {
    id: Uuid,
    runtime_execution_id: Uuid,
    stage_record_id: Uuid,
    action_kind_key: String,
    action_ordinal: i32,
    action_state_key: String,
    provider_binding_id: Option<Uuid>,
    tool_name: Option<String>,
    usage_json: Option<serde_json::Value>,
    summary_json: serde_json::Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct RuntimeActionRecordRow {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Uuid,
    pub action_kind: RuntimeActionKind,
    pub action_ordinal: i32,
    pub action_state: RuntimeActionState,
    pub provider_binding_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub usage_json: Option<serde_json::Value>,
    pub summary_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct RuntimePolicyDecisionRowRecord {
    id: Uuid,
    runtime_execution_id: Uuid,
    stage_record_id: Option<Uuid>,
    action_record_id: Option<Uuid>,
    target_kind_key: String,
    decision_kind_key: String,
    reason_code: String,
    reason_summary_redacted: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct RuntimePolicyDecisionRow {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Option<Uuid>,
    pub action_record_id: Option<Uuid>,
    pub target_kind: RuntimeDecisionTargetKind,
    pub decision_kind: RuntimeDecisionKind,
    pub reason_code: String,
    pub reason_summary_redacted: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewRuntimeExecution<'a> {
    pub id: Uuid,
    pub owner_kind: &'a str,
    pub owner_id: Uuid,
    pub task_kind: &'a str,
    pub surface_kind: &'a str,
    pub contract_name: &'a str,
    pub contract_version: &'a str,
    pub lifecycle_state: &'a str,
    pub active_stage: Option<&'a str>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<&'a str>,
    pub failure_summary_redacted: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct UpdateRuntimeExecution<'a> {
    pub lifecycle_state: &'a str,
    pub active_stage: Option<&'a str>,
    pub turn_count: i32,
    pub failure_code: Option<&'a str>,
    pub failure_summary_redacted: Option<&'a str>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewRuntimeStageRecord<'a> {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_kind: &'a str,
    pub stage_ordinal: i32,
    pub attempt_no: i32,
    pub stage_state: &'a str,
    pub deterministic: bool,
    pub input_summary_json: serde_json::Value,
    pub output_summary_json: serde_json::Value,
    pub failure_code: Option<&'a str>,
    pub failure_summary_redacted: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewRuntimeActionRecord<'a> {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Uuid,
    pub action_kind: &'a str,
    pub action_ordinal: i32,
    pub action_state: &'a str,
    pub provider_binding_id: Option<Uuid>,
    pub tool_name: Option<&'a str>,
    pub usage_json: Option<serde_json::Value>,
    pub summary_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct NewRuntimePolicyDecision<'a> {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Option<Uuid>,
    pub action_record_id: Option<Uuid>,
    pub target_kind: &'a str,
    pub decision_kind: &'a str,
    pub reason_code: &'a str,
    pub reason_summary_redacted: &'a str,
}

pub async fn create_runtime_execution(
    postgres: &PgPool,
    input: &NewRuntimeExecution<'_>,
) -> Result<RuntimeExecutionRow, sqlx::Error> {
    let row = sqlx::query_as::<_, RuntimeExecutionRowRecord>(
        "insert into runtime_execution (
            id, owner_kind, owner_id, task_kind, surface_kind, contract_name, contract_version,
            lifecycle_state, active_stage, turn_budget, turn_count, parallel_action_limit,
            failure_code, failure_summary_redacted, accepted_at, completed_at
         ) values (
            $1, $2::runtime_execution_owner_kind, $3, $4::runtime_task_kind,
            $5::runtime_surface_kind, $6, $7, $8::runtime_lifecycle_state,
            $9::runtime_stage_kind, $10, $11, $12, $13, $14, now(), null
         )
         returning
            id,
            owner_kind::text as owner_kind_key,
            owner_id,
            task_kind::text as task_kind_key,
            surface_kind::text as surface_kind_key,
            contract_name,
            contract_version,
            lifecycle_state::text as lifecycle_state_key,
            active_stage::text as active_stage_key,
            turn_budget,
            turn_count,
            parallel_action_limit,
            failure_code,
            failure_summary_redacted,
            accepted_at,
            completed_at",
    )
    .bind(input.id)
    .bind(input.owner_kind)
    .bind(input.owner_id)
    .bind(input.task_kind)
    .bind(input.surface_kind)
    .bind(input.contract_name)
    .bind(input.contract_version)
    .bind(input.lifecycle_state)
    .bind(input.active_stage)
    .bind(input.turn_budget)
    .bind(input.turn_count)
    .bind(input.parallel_action_limit)
    .bind(input.failure_code)
    .bind(input.failure_summary_redacted)
    .fetch_one(postgres)
    .await?;
    map_runtime_execution_row(row)
}

pub async fn get_runtime_execution_by_id(
    postgres: &PgPool,
    runtime_execution_id: Uuid,
) -> Result<Option<RuntimeExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExecutionRowRecord>(
        "select
            id,
            owner_kind::text as owner_kind_key,
            owner_id,
            task_kind::text as task_kind_key,
            surface_kind::text as surface_kind_key,
            contract_name,
            contract_version,
            lifecycle_state::text as lifecycle_state_key,
            active_stage::text as active_stage_key,
            turn_budget,
            turn_count,
            parallel_action_limit,
            failure_code,
            failure_summary_redacted,
            accepted_at,
            completed_at
         from runtime_execution
         where id = $1",
    )
    .bind(runtime_execution_id)
    .fetch_optional(postgres)
    .await?
    .map(map_runtime_execution_row)
    .transpose()
}

pub async fn update_runtime_execution(
    postgres: &PgPool,
    runtime_execution_id: Uuid,
    input: &UpdateRuntimeExecution<'_>,
) -> Result<Option<RuntimeExecutionRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, RuntimeExecutionRowRecord>(
        "update runtime_execution
         set lifecycle_state = $2::runtime_lifecycle_state,
             active_stage = $3::runtime_stage_kind,
             turn_count = $4,
             failure_code = $5,
             failure_summary_redacted = $6,
             completed_at = $7
         where id = $1
         returning
            id,
            owner_kind::text as owner_kind_key,
            owner_id,
            task_kind::text as task_kind_key,
            surface_kind::text as surface_kind_key,
            contract_name,
            contract_version,
            lifecycle_state::text as lifecycle_state_key,
            active_stage::text as active_stage_key,
            turn_budget,
            turn_count,
            parallel_action_limit,
            failure_code,
            failure_summary_redacted,
            accepted_at,
            completed_at",
    )
    .bind(runtime_execution_id)
    .bind(input.lifecycle_state)
    .bind(input.active_stage)
    .bind(input.turn_count)
    .bind(input.failure_code)
    .bind(input.failure_summary_redacted)
    .bind(input.completed_at)
    .fetch_optional(postgres)
    .await?;
    row.map(map_runtime_execution_row).transpose()
}

pub async fn list_runtime_executions_by_owner(
    postgres: &PgPool,
    owner_kind: &str,
    owner_id: Uuid,
) -> Result<Vec<RuntimeExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExecutionRowRecord>(
        "select
            id,
            owner_kind::text as owner_kind_key,
            owner_id,
            task_kind::text as task_kind_key,
            surface_kind::text as surface_kind_key,
            contract_name,
            contract_version,
            lifecycle_state::text as lifecycle_state_key,
            active_stage::text as active_stage_key,
            turn_budget,
            turn_count,
            parallel_action_limit,
            failure_code,
            failure_summary_redacted,
            accepted_at,
            completed_at
         from runtime_execution
         where owner_kind = $1::runtime_execution_owner_kind
           and owner_id = $2
         order by accepted_at desc, id desc",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_runtime_execution_row)
    .collect()
}

pub async fn create_runtime_stage_record(
    postgres: &PgPool,
    input: &NewRuntimeStageRecord<'_>,
) -> Result<RuntimeStageRecordRow, sqlx::Error> {
    let row = sqlx::query_as::<_, RuntimeStageRecordRowRecord>(
        "insert into runtime_stage_record (
            id, runtime_execution_id, stage_kind, stage_ordinal, attempt_no, stage_state,
            deterministic, started_at, completed_at, input_summary_json, output_summary_json,
            failure_code, failure_summary_redacted
         ) values (
            $1, $2, $3::runtime_stage_kind, $4, $5, $6::runtime_stage_state, $7,
            now(), now(), $8, $9, $10, $11
         )
         returning
            id,
            runtime_execution_id,
            stage_kind::text as stage_kind_key,
            stage_ordinal,
            attempt_no,
            stage_state::text as stage_state_key,
            deterministic,
            started_at,
            completed_at,
            input_summary_json,
            output_summary_json,
            failure_code,
            failure_summary_redacted",
    )
    .bind(input.id)
    .bind(input.runtime_execution_id)
    .bind(input.stage_kind)
    .bind(input.stage_ordinal)
    .bind(input.attempt_no)
    .bind(input.stage_state)
    .bind(input.deterministic)
    .bind(input.input_summary_json.clone())
    .bind(input.output_summary_json.clone())
    .bind(input.failure_code)
    .bind(input.failure_summary_redacted)
    .fetch_one(postgres)
    .await?;
    map_runtime_stage_record_row(row)
}

pub async fn list_runtime_stage_records(
    postgres: &PgPool,
    runtime_execution_id: Uuid,
) -> Result<Vec<RuntimeStageRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeStageRecordRowRecord>(
        "select
            id,
            runtime_execution_id,
            stage_kind::text as stage_kind_key,
            stage_ordinal,
            attempt_no,
            stage_state::text as stage_state_key,
            deterministic,
            started_at,
            completed_at,
            input_summary_json,
            output_summary_json,
            failure_code,
            failure_summary_redacted
         from runtime_stage_record
         where runtime_execution_id = $1
         order by stage_ordinal asc, attempt_no asc, id asc",
    )
    .bind(runtime_execution_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_runtime_stage_record_row)
    .collect()
}

pub async fn create_runtime_action_record(
    postgres: &PgPool,
    input: &NewRuntimeActionRecord<'_>,
) -> Result<RuntimeActionRecordRow, sqlx::Error> {
    let row = sqlx::query_as::<_, RuntimeActionRecordRowRecord>(
        "insert into runtime_action_record (
            id, runtime_execution_id, stage_record_id, action_kind, action_ordinal, action_state,
            provider_binding_id, tool_name, usage_json, summary_json, created_at
         ) values (
            $1, $2, $3, $4::runtime_action_kind, $5, $6::runtime_action_state, $7, $8, $9, $10, now()
         )
         returning
            id,
            runtime_execution_id,
            stage_record_id,
            action_kind::text as action_kind_key,
            action_ordinal,
            action_state::text as action_state_key,
            provider_binding_id,
            tool_name,
            usage_json,
            summary_json,
            created_at",
    )
    .bind(input.id)
    .bind(input.runtime_execution_id)
    .bind(input.stage_record_id)
    .bind(input.action_kind)
    .bind(input.action_ordinal)
    .bind(input.action_state)
    .bind(input.provider_binding_id)
    .bind(input.tool_name)
    .bind(input.usage_json.clone())
    .bind(input.summary_json.clone())
    .fetch_one(postgres)
    .await?;
    map_runtime_action_record_row(row)
}

pub async fn list_runtime_action_records(
    postgres: &PgPool,
    runtime_execution_id: Uuid,
) -> Result<Vec<RuntimeActionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeActionRecordRowRecord>(
        "select
            id,
            runtime_execution_id,
            stage_record_id,
            action_kind::text as action_kind_key,
            action_ordinal,
            action_state::text as action_state_key,
            provider_binding_id,
            tool_name,
            usage_json,
            summary_json,
            created_at
         from runtime_action_record
         where runtime_execution_id = $1
         order by action_ordinal asc, id asc",
    )
    .bind(runtime_execution_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_runtime_action_record_row)
    .collect()
}

pub async fn create_runtime_policy_decision(
    postgres: &PgPool,
    input: &NewRuntimePolicyDecision<'_>,
) -> Result<RuntimePolicyDecisionRow, sqlx::Error> {
    let row = sqlx::query_as::<_, RuntimePolicyDecisionRowRecord>(
        "insert into runtime_policy_decision (
            id, runtime_execution_id, stage_record_id, action_record_id, target_kind,
            decision_kind, reason_code, reason_summary_redacted, created_at
         ) values (
            $1, $2, $3, $4, $5::runtime_decision_target_kind,
            $6::runtime_decision_kind, $7, $8, now()
         )
         returning
            id,
            runtime_execution_id,
            stage_record_id,
            action_record_id,
            target_kind::text as target_kind_key,
            decision_kind::text as decision_kind_key,
            reason_code,
            reason_summary_redacted,
            created_at",
    )
    .bind(input.id)
    .bind(input.runtime_execution_id)
    .bind(input.stage_record_id)
    .bind(input.action_record_id)
    .bind(input.target_kind)
    .bind(input.decision_kind)
    .bind(input.reason_code)
    .bind(input.reason_summary_redacted)
    .fetch_one(postgres)
    .await?;
    map_runtime_policy_decision_row(row)
}

pub async fn list_runtime_policy_decisions(
    postgres: &PgPool,
    runtime_execution_id: Uuid,
) -> Result<Vec<RuntimePolicyDecisionRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimePolicyDecisionRowRecord>(
        "select
            id,
            runtime_execution_id,
            stage_record_id,
            action_record_id,
            target_kind::text as target_kind_key,
            decision_kind::text as decision_kind_key,
            reason_code,
            reason_summary_redacted,
            created_at
         from runtime_policy_decision
         where runtime_execution_id = $1
         order by created_at asc, id asc",
    )
    .bind(runtime_execution_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_runtime_policy_decision_row)
    .collect()
}

fn map_runtime_execution_row(
    row: RuntimeExecutionRowRecord,
) -> Result<RuntimeExecutionRow, sqlx::Error> {
    Ok(RuntimeExecutionRow {
        id: row.id,
        owner_kind: parse_runtime_execution_owner_kind(&row.owner_kind_key)?,
        owner_id: row.owner_id,
        task_kind: parse_runtime_task_kind(&row.task_kind_key)?,
        surface_kind: parse_runtime_surface_kind(&row.surface_kind_key)?,
        contract_name: row.contract_name,
        contract_version: row.contract_version,
        lifecycle_state: parse_runtime_lifecycle_state(&row.lifecycle_state_key)?,
        active_stage: row.active_stage_key.as_deref().map(parse_runtime_stage_kind).transpose()?,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
        accepted_at: row.accepted_at,
        completed_at: row.completed_at,
    })
}

fn map_runtime_stage_record_row(
    row: RuntimeStageRecordRowRecord,
) -> Result<RuntimeStageRecordRow, sqlx::Error> {
    Ok(RuntimeStageRecordRow {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_kind: parse_runtime_stage_kind(&row.stage_kind_key)?,
        stage_ordinal: row.stage_ordinal,
        attempt_no: row.attempt_no,
        stage_state: parse_runtime_stage_state(&row.stage_state_key)?,
        deterministic: row.deterministic,
        started_at: row.started_at,
        completed_at: row.completed_at,
        input_summary_json: row.input_summary_json,
        output_summary_json: row.output_summary_json,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
    })
}

fn map_runtime_action_record_row(
    row: RuntimeActionRecordRowRecord,
) -> Result<RuntimeActionRecordRow, sqlx::Error> {
    Ok(RuntimeActionRecordRow {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_kind: parse_runtime_action_kind(&row.action_kind_key)?,
        action_ordinal: row.action_ordinal,
        action_state: parse_runtime_action_state(&row.action_state_key)?,
        provider_binding_id: row.provider_binding_id,
        tool_name: row.tool_name,
        usage_json: row.usage_json,
        summary_json: row.summary_json,
        created_at: row.created_at,
    })
}

fn map_runtime_policy_decision_row(
    row: RuntimePolicyDecisionRowRecord,
) -> Result<RuntimePolicyDecisionRow, sqlx::Error> {
    Ok(RuntimePolicyDecisionRow {
        id: row.id,
        runtime_execution_id: row.runtime_execution_id,
        stage_record_id: row.stage_record_id,
        action_record_id: row.action_record_id,
        target_kind: parse_runtime_decision_target_kind(&row.target_kind_key)?,
        decision_kind: parse_runtime_decision_kind(&row.decision_kind_key)?,
        reason_code: row.reason_code,
        reason_summary_redacted: row.reason_summary_redacted,
        created_at: row.created_at,
    })
}

fn parse_runtime_execution_owner_kind(
    value: &str,
) -> Result<RuntimeExecutionOwnerKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_task_kind(value: &str) -> Result<RuntimeTaskKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_surface_kind(value: &str) -> Result<RuntimeSurfaceKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_lifecycle_state(value: &str) -> Result<RuntimeLifecycleState, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_stage_kind(value: &str) -> Result<RuntimeStageKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_stage_state(value: &str) -> Result<RuntimeStageState, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_action_kind(value: &str) -> Result<RuntimeActionKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_action_state(value: &str) -> Result<RuntimeActionState, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_decision_target_kind(
    value: &str,
) -> Result<RuntimeDecisionTargetKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_decision_kind(value: &str) -> Result<RuntimeDecisionKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn invalid_enum_value(message: String) -> sqlx::Error {
    sqlx::Error::Protocol(message)
}
