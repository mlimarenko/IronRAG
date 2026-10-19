use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::domains::{
    agent_runtime::{RuntimeLifecycleState, RuntimeStageKind, RuntimeSurfaceKind},
    query::{QueryConversationState, QueryTurnKind},
};

pub const INTERRUPTED_QUERY_EXECUTION_FAILURE_CODE: &str = "query_execution_interrupted";

const MCP_REQUEST_SURFACE: &str = RuntimeSurfaceKind::Mcp.as_str();
const CONVERSATION_RETENTION_LOCK_SQL: &str =
    "select pg_advisory_xact_lock(hashtextextended(concat($1::text, ':', $2::text), 0))";
// Both windows of the same race need a grace period, not just one. The empty
// branch below has always aged conversations 10 minutes from creation so a
// conversation is not reaped before its first execution row lands. The
// completed branch needs the symmetric guard from completion: a tool call
// protects only its OWN conversation while pruning, so under concurrent load
// turn B's prune could delete turn A's just-completed child conversation
// before A's parent attached its grounding — the cascade took the child's
// `query_execution` row and A answered with an empty reference list.
const MCP_CONVERSATION_OVERFLOW_DELETE_SQL: &str = "delete from query_conversation
     where id in (
         select conversation.id
         from query_conversation conversation
         where conversation.library_id = $1
           and conversation.request_surface = $2::surface_kind
           and ($4::uuid is null or conversation.id <> $4)
           and (
               (
                   exists (
                       select 1
                       from query_execution execution
                       where execution.conversation_id = conversation.id
                   )
                   and not exists (
                       select 1
                       from query_execution execution
                       join runtime_execution runtime
                         on runtime.id = execution.runtime_execution_id
                        and runtime.owner_kind = 'query_execution'
                        and runtime.owner_id = execution.id
                       where execution.conversation_id = conversation.id
                         and (
                            runtime.lifecycle_state in ('accepted', 'running')
                            or coalesce(runtime.completed_at, runtime.accepted_at)
                               > now() - interval '10 minutes'
                         )
                   )
               )
               or (
                   not exists (
                       select 1
                       from query_execution execution
                       where execution.conversation_id = conversation.id
                   )
                   and conversation.created_at < now() - interval '10 minutes'
               )
           )
           and not exists (
               select 1
               from query_execution source_execution
               join query_execution_replay replay
                 on replay.source_execution_id = source_execution.id
               where source_execution.conversation_id = conversation.id
                 and replay.conversation_id <> conversation.id
           )
         order by conversation.created_at asc, conversation.id asc
         limit $3
         for update skip locked
     )";
const AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL: &str = "delete from query_conversation
     where id in (
         select conversation.id
         from query_conversation conversation
         where conversation.library_id = $1
           and conversation.request_surface = $2::surface_kind
           and ($4::uuid is null or conversation.id <> $4)
           and conversation.created_at < now() - interval '10 minutes'
           and not exists (
               select 1
               from query_execution execution
               join runtime_execution runtime
                 on runtime.id = execution.runtime_execution_id
                and runtime.owner_kind = 'query_execution'
                and runtime.owner_id = execution.id
               where execution.conversation_id = conversation.id
                 and runtime.lifecycle_state in ('accepted', 'running')
           )
           and not exists (
               select 1
               from query_execution source_execution
               join query_execution_replay replay
                 on replay.source_execution_id = source_execution.id
               where source_execution.conversation_id = conversation.id
                 and replay.conversation_id <> conversation.id
           )
         order by conversation.created_at asc, conversation.id asc
         limit $3
         for update skip locked
     )";

macro_rules! query_execution_runtime_sql {
    ($suffix:literal) => {
        concat!(
            "select
                execution.id,
                execution.context_bundle_id,
                execution.workspace_id,
                execution.library_id,
                execution.conversation_id,
                execution.request_turn_id,
                execution.response_turn_id,
                execution.binding_id,
                execution.runtime_execution_id,
                runtime.lifecycle_state::text as runtime_lifecycle_state_text,
                runtime.active_stage::text as runtime_active_stage_text,
                runtime.turn_budget,
                runtime.turn_count,
                runtime.parallel_action_limit,
                execution.query_text,
                coalesce(runtime.failure_code, execution.failure_code) as failure_code,
                runtime.failure_summary_redacted,
                execution.started_at,
                runtime.accepted_at as runtime_accepted_at,
                runtime.completed_at as runtime_completed_at
            from query_execution execution
            join runtime_execution runtime
              on runtime.id = execution.runtime_execution_id
             and runtime.owner_kind = 'query_execution'
             and runtime.owner_id = execution.id",
            $suffix
        )
    };
}

#[derive(Debug, Clone, FromRow)]
struct StaleQueryExecutionCandidate {
    execution_id: Uuid,
    runtime_execution_id: Uuid,
    async_operation_id: Uuid,
}

#[derive(Debug, Clone, FromRow)]
struct QueryConversationRowRecord {
    id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    created_by_principal_id: Option<Uuid>,
    title: Option<String>,
    conversation_state_text: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryConversationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: QueryConversationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct QueryTurnRowRecord {
    id: Uuid,
    conversation_id: Uuid,
    turn_index: i32,
    turn_kind_text: String,
    author_principal_id: Option<Uuid>,
    content_text: String,
    execution_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryTurnRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: QueryTurnKind,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub execution_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct QueryExecutionRowRecord {
    id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    conversation_id: Uuid,
    context_bundle_id: Uuid,
    request_turn_id: Option<Uuid>,
    response_turn_id: Option<Uuid>,
    binding_id: Option<Uuid>,
    runtime_execution_id: Uuid,
    runtime_lifecycle_state_text: String,
    runtime_active_stage_text: Option<String>,
    turn_budget: i32,
    turn_count: i32,
    parallel_action_limit: i32,
    query_text: String,
    failure_code: Option<String>,
    failure_summary_redacted: Option<String>,
    started_at: DateTime<Utc>,
    runtime_accepted_at: DateTime<Utc>,
    runtime_completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryExecutionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub context_bundle_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub runtime_execution_id: Uuid,
    pub runtime_lifecycle_state: RuntimeLifecycleState,
    pub runtime_active_stage: Option<RuntimeStageKind>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub query_text: String,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub started_at: DateTime<Utc>,
    pub runtime_accepted_at: DateTime<Utc>,
    pub runtime_completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewQueryConversation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<&'a str>,
    pub conversation_state: &'a str,
    /// Stable `surface_kind` enum value — `'ui'` for the web
    /// assistant session-create path, `'mcp'` for the `grounded_answer`
    /// tool. Set once at creation time and drives the UI session
    /// listing filter so MCP-born conversations do not leak into the
    /// web surface.
    pub request_surface: &'a str,
}

#[derive(Debug, Clone)]
pub struct NewQueryTurn<'a> {
    pub conversation_id: Uuid,
    pub turn_kind: &'a str,
    pub author_principal_id: Option<Uuid>,
    pub content_text: &'a str,
    pub execution_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewQueryExecution<'a> {
    pub execution_id: Uuid,
    pub context_bundle_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub runtime_execution_id: Uuid,
    pub query_text: &'a str,
    pub failure_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct UpdateQueryExecution {
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Atomic result of deleting one durable UI conversation.
pub enum DeleteQueryConversationOutcome {
    /// The conversation and its query-owned children were removed.
    Deleted,
    /// The row was absent, non-UI, or outside the caller's mutation ownership.
    NotFoundOrForbidden,
    /// At least one execution has not reached its terminal state.
    ActiveExecution,
    /// Another conversation still depends on an execution as replay provenance.
    RetainedByExternalReplay,
}

fn map_query_conversation_row(
    row: QueryConversationRowRecord,
) -> Result<QueryConversationRow, sqlx::Error> {
    Ok(QueryConversationRow {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        created_by_principal_id: row.created_by_principal_id,
        title: row.title,
        conversation_state: parse_query_conversation_state(&row.conversation_state_text)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn map_query_turn_row(row: QueryTurnRowRecord) -> Result<QueryTurnRow, sqlx::Error> {
    Ok(QueryTurnRow {
        id: row.id,
        conversation_id: row.conversation_id,
        turn_index: row.turn_index,
        turn_kind: parse_query_turn_kind(&row.turn_kind_text)?,
        author_principal_id: row.author_principal_id,
        content_text: row.content_text,
        execution_id: row.execution_id,
        created_at: row.created_at,
    })
}

fn map_query_execution_row(row: QueryExecutionRowRecord) -> Result<QueryExecutionRow, sqlx::Error> {
    Ok(QueryExecutionRow {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        conversation_id: row.conversation_id,
        context_bundle_id: row.context_bundle_id,
        request_turn_id: row.request_turn_id,
        response_turn_id: row.response_turn_id,
        binding_id: row.binding_id,
        runtime_execution_id: row.runtime_execution_id,
        runtime_lifecycle_state: parse_runtime_lifecycle_state(&row.runtime_lifecycle_state_text)?,
        runtime_active_stage: row
            .runtime_active_stage_text
            .as_deref()
            .map(parse_runtime_stage_kind)
            .transpose()?,
        turn_budget: row.turn_budget,
        turn_count: row.turn_count,
        parallel_action_limit: row.parallel_action_limit,
        query_text: row.query_text,
        failure_code: row.failure_code,
        failure_summary_redacted: row.failure_summary_redacted,
        started_at: row.started_at,
        runtime_accepted_at: row.runtime_accepted_at,
        runtime_completed_at: row.runtime_completed_at,
    })
}

fn parse_query_conversation_state(value: &str) -> Result<QueryConversationState, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_query_turn_kind(value: &str) -> Result<QueryTurnKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_lifecycle_state(value: &str) -> Result<RuntimeLifecycleState, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

fn parse_runtime_stage_kind(value: &str) -> Result<RuntimeStageKind, sqlx::Error> {
    value.parse().map_err(invalid_enum_value)
}

const fn invalid_enum_value(message: String) -> sqlx::Error {
    sqlx::Error::Protocol(message)
}

mod conversations;
mod executions;
mod references;
mod turns;

pub use self::{conversations::*, executions::*, references::*, turns::*};

#[cfg(test)]
mod tests;
