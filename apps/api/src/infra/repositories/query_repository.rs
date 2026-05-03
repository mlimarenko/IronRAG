#![allow(clippy::missing_const_for_fn)]

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domains::{
    agent_runtime::{RuntimeLifecycleState, RuntimeStageKind},
    query::{QueryConversationState, QueryTurnKind},
};

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
    completed_at: Option<DateTime<Utc>>,
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
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewQueryConversation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<&'a str>,
    pub conversation_state: &'a str,
    /// Canonical `surface_kind` enum value — `'ui'` for the web
    /// assistant session-create path, `'mcp'` for the grounded_answer
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
pub struct UpdateQueryExecution<'a> {
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub failure_code: Option<&'a str>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub async fn list_conversations_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<QueryConversationRow>, sqlx::Error> {
    // UI-only listing: MCP-born conversations are audit-visible but
    // must not surface in the web assistant session list. The surface
    // is set at creation time on `request_surface`.
    sqlx::query_as::<_, QueryConversationRowRecord>(
        "select
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state_text,
            created_at,
            updated_at
         from query_conversation
         where library_id = $1
           and request_surface = 'ui'
         order by updated_at desc, created_at desc
         limit 5",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_conversation_row)
    .collect()
}

pub async fn get_conversation_by_id(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Option<QueryConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryConversationRowRecord>(
        "select
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state_text,
            created_at,
            updated_at
         from query_conversation
         where id = $1",
    )
    .bind(conversation_id)
    .fetch_optional(postgres)
    .await?
    .map(map_query_conversation_row)
    .transpose()
}

pub async fn create_conversation(
    postgres: &PgPool,
    input: &NewQueryConversation<'_>,
    max_library_conversations: usize,
) -> Result<QueryConversationRow, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let existing_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from query_conversation
         where library_id = $1
           and request_surface = $2::surface_kind
         order by created_at asc, id asc",
    )
    .bind(input.library_id)
    .bind(input.request_surface)
    .fetch_all(&mut *transaction)
    .await?;

    let overflow_count =
        existing_ids.len().saturating_add(1).saturating_sub(max_library_conversations);

    if overflow_count > 0 {
        let prune_ids = existing_ids.into_iter().take(overflow_count).collect::<Vec<_>>();
        sqlx::query("delete from query_conversation where id = any($1)")
            .bind(&prune_ids)
            .execute(&mut *transaction)
            .await?;
    }

    let row = sqlx::query_as::<_, QueryConversationRowRecord>(
        "insert into query_conversation (
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state,
            request_surface,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6::query_conversation_state, $7::surface_kind, now(), now())
        returning
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state_text,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.created_by_principal_id)
    .bind(input.title)
    .bind(input.conversation_state)
    .bind(input.request_surface)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    map_query_conversation_row(row)
}

pub async fn update_conversation_title(
    postgres: &PgPool,
    conversation_id: Uuid,
    title: &str,
) -> Result<QueryConversationRow, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryConversationRowRecord>(
        "update query_conversation
         set title = $2,
             updated_at = now()
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state_text,
            created_at,
            updated_at",
    )
    .bind(conversation_id)
    .bind(title)
    .fetch_one(postgres)
    .await?;
    map_query_conversation_row(row)
}

pub async fn list_turns_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryTurnRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryTurnRowRecord>(
        "select
            id,
            conversation_id,
            turn_index,
            turn_kind::text as turn_kind_text,
            author_principal_id,
            content_text,
            execution_id,
            created_at
         from query_turn
         where conversation_id = $1
         order by created_at asc, turn_index asc
         limit 200",
    )
    .bind(conversation_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_turn_row)
    .collect()
}

pub async fn get_turn_by_id(
    postgres: &PgPool,
    turn_id: Uuid,
) -> Result<Option<QueryTurnRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryTurnRowRecord>(
        "select
            id,
            conversation_id,
            turn_index,
            turn_kind::text as turn_kind_text,
            author_principal_id,
            content_text,
            execution_id,
            created_at
         from query_turn
         where id = $1",
    )
    .bind(turn_id)
    .fetch_optional(postgres)
    .await?
    .map(map_query_turn_row)
    .transpose()
}

pub async fn create_turn(
    postgres: &PgPool,
    input: &NewQueryTurn<'_>,
) -> Result<QueryTurnRow, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryTurnRowRecord>(
        "with locked_conversation as (
            update query_conversation
            set updated_at = now()
            where id = $1
            returning id
        ),
        next_turn as (
            select coalesce(max(turn_index) + 1, 1) as turn_index
            from query_turn
            where conversation_id = $1
        )
        insert into query_turn (
            id,
            conversation_id,
            turn_index,
            turn_kind,
            author_principal_id,
            content_text,
            execution_id,
            created_at
        )
        select
            $2,
            $1,
            next_turn.turn_index,
            $3::query_turn_kind,
            $4,
            $5,
            $6,
            now()
        from locked_conversation, next_turn
        returning
            id,
            conversation_id,
            turn_index,
            turn_kind::text as turn_kind_text,
            author_principal_id,
            content_text,
            execution_id,
            created_at",
    )
    .bind(input.conversation_id)
    .bind(Uuid::now_v7())
    .bind(input.turn_kind)
    .bind(input.author_principal_id)
    .bind(input.content_text)
    .bind(input.execution_id)
    .fetch_one(postgres)
    .await?;
    map_query_turn_row(row)
}

pub async fn list_executions_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRowRecord>(
        "select
            id,
            context_bundle_id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            runtime_execution_id,
            runtime_lifecycle_state_text,
            runtime_active_stage_text,
            turn_budget,
            turn_count,
            parallel_action_limit,
            query_text,
            failure_code,
            failure_summary_redacted,
            started_at,
            completed_at
         from (
            select
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
                coalesce(runtime.completed_at, execution.completed_at) as completed_at
            from query_execution execution
            join runtime_execution runtime on runtime.id = execution.runtime_execution_id
         ) execution_view
         where conversation_id = $1
         order by started_at desc, id desc",
    )
    .bind(conversation_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_execution_row)
    .collect()
}

pub async fn get_execution_by_id(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRowRecord>(
        "select
            id,
            context_bundle_id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            runtime_execution_id,
            runtime_lifecycle_state_text,
            runtime_active_stage_text,
            turn_budget,
            turn_count,
            parallel_action_limit,
            query_text,
            failure_code,
            failure_summary_redacted,
            started_at,
            completed_at
         from (
            select
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
                coalesce(runtime.completed_at, execution.completed_at) as completed_at
            from query_execution execution
            join runtime_execution runtime on runtime.id = execution.runtime_execution_id
         ) execution_view
         where id = $1",
    )
    .bind(execution_id)
    .fetch_optional(postgres)
    .await?
    .map(map_query_execution_row)
    .transpose()
}

pub async fn list_executions_by_ids(
    postgres: &PgPool,
    execution_ids: &[Uuid],
) -> Result<Vec<QueryExecutionRow>, sqlx::Error> {
    if execution_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, QueryExecutionRowRecord>(
        "select
            id,
            context_bundle_id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            runtime_execution_id,
            runtime_lifecycle_state_text,
            runtime_active_stage_text,
            turn_budget,
            turn_count,
            parallel_action_limit,
            query_text,
            failure_code,
            failure_summary_redacted,
            started_at,
            completed_at
         from (
            select
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
                coalesce(runtime.completed_at, execution.completed_at) as completed_at
            from query_execution execution
            join runtime_execution runtime on runtime.id = execution.runtime_execution_id
         ) execution_view
         where id = any($1)
         order by started_at desc, id desc",
    )
    .bind(execution_ids)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_execution_row)
    .collect()
}

pub async fn create_execution(
    postgres: &PgPool,
    input: &NewQueryExecution<'_>,
) -> Result<QueryExecutionRow, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryExecutionRowRecord>(
        "with inserted as (
            insert into query_execution (
                id,
                workspace_id,
                library_id,
                conversation_id,
                context_bundle_id,
                request_turn_id,
                response_turn_id,
                binding_id,
                runtime_execution_id,
                query_text,
                failure_code,
                started_at,
                completed_at
            )
            values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, now(), null
            )
            returning *
        )
        select
            inserted.id,
            inserted.context_bundle_id,
            inserted.workspace_id,
            inserted.library_id,
            inserted.conversation_id,
            inserted.request_turn_id,
            inserted.response_turn_id,
            inserted.binding_id,
            inserted.runtime_execution_id,
            runtime.lifecycle_state::text as runtime_lifecycle_state_text,
            runtime.active_stage::text as runtime_active_stage_text,
            runtime.turn_budget,
            runtime.turn_count,
            runtime.parallel_action_limit,
            inserted.query_text,
            coalesce(runtime.failure_code, inserted.failure_code) as failure_code,
            runtime.failure_summary_redacted,
            inserted.started_at,
            coalesce(runtime.completed_at, inserted.completed_at) as completed_at
        from inserted
        join runtime_execution runtime on runtime.id = inserted.runtime_execution_id",
    )
    .bind(input.execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.context_bundle_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.binding_id)
    .bind(input.runtime_execution_id)
    .bind(input.query_text)
    .bind(input.failure_code)
    .fetch_one(postgres)
    .await?;
    map_query_execution_row(row)
}

/// One row that will land in `query_chunk_reference`. The turn layer
/// captures these in `RuntimeStructuredQueryResult.chunk_references`
/// and forwards them to `append_chunk_references` once it has an
/// `execution_id`. Keeping the repo-layer type small and insert-only
/// avoids leaking the internal `RuntimeMatchedChunk` into the query
/// repository surface.
#[derive(Debug, Clone, Copy)]
pub struct NewQueryChunkReference {
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

/// Persist the final ranked chunks that shaped a query execution's
/// answer context into `query_chunk_reference`. The write is an UNNEST
/// batch insert — one Postgres round-trip regardless of how many
/// chunks landed in the bundle. `ON CONFLICT DO NOTHING` makes the
/// call idempotent if the turn layer ever retries after a transient
/// failure on a later step (the caller holds the execution_id so a
/// replay cannot produce mismatched rows).
///
/// No-op when `references` is empty — avoids a redundant round-trip
/// on turns that produced no retrieved chunks.
pub async fn append_chunk_references(
    postgres: &PgPool,
    execution_id: Uuid,
    references: &[NewQueryChunkReference],
) -> Result<u64, sqlx::Error> {
    if references.is_empty() {
        return Ok(0);
    }
    let chunk_ids: Vec<Uuid> = references.iter().map(|reference| reference.chunk_id).collect();
    let ranks: Vec<i32> = references.iter().map(|reference| reference.rank).collect();
    let scores: Vec<f64> = references.iter().map(|reference| reference.score).collect();
    let result = sqlx::query(
        "insert into query_chunk_reference (execution_id, chunk_id, rank, score)
         select $1, chunk_id, rank, score
         from unnest($2::uuid[], $3::int[], $4::double precision[])
              as input(chunk_id, rank, score)
         on conflict (execution_id, chunk_id) do nothing",
    )
    .bind(execution_id)
    .bind(&chunk_ids)
    .bind(&ranks)
    .bind(&scores)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

pub async fn update_execution(
    postgres: &PgPool,
    execution_id: Uuid,
    input: &UpdateQueryExecution<'_>,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryExecutionRowRecord>(
        "with updated as (
            update query_execution
             set request_turn_id = $2,
                 response_turn_id = $3,
                 failure_code = $4,
                 completed_at = $5
             where id = $1
             returning *
        )
        select
            updated.id,
            updated.context_bundle_id,
            updated.workspace_id,
            updated.library_id,
            updated.conversation_id,
            updated.request_turn_id,
            updated.response_turn_id,
            updated.binding_id,
            updated.runtime_execution_id,
            runtime.lifecycle_state::text as runtime_lifecycle_state_text,
            runtime.active_stage::text as runtime_active_stage_text,
            runtime.turn_budget,
            runtime.turn_count,
            runtime.parallel_action_limit,
            updated.query_text,
            coalesce(runtime.failure_code, updated.failure_code) as failure_code,
            runtime.failure_summary_redacted,
            updated.started_at,
            coalesce(runtime.completed_at, updated.completed_at) as completed_at
        from updated
        join runtime_execution runtime on runtime.id = updated.runtime_execution_id",
    )
    .bind(execution_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.failure_code)
    .bind(input.completed_at)
    .fetch_optional(postgres)
    .await?;
    row.map(map_query_execution_row).transpose()
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
        completed_at: row.completed_at,
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

fn invalid_enum_value(message: String) -> sqlx::Error {
    sqlx::Error::Protocol(message)
}
