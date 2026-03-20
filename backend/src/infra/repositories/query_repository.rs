use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct QueryConversationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
    pub conversation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryTurnRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub turn_index: i32,
    pub turn_kind: String,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub execution_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryExecutionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub execution_state: String,
    pub query_text: String,
    pub failure_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryChunkReferenceRow {
    pub execution_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryGraphNodeReferenceRow {
    pub execution_id: Uuid,
    pub node_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryGraphEdgeReferenceRow {
    pub execution_id: Uuid,
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct NewQueryConversation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<&'a str>,
    pub conversation_state: &'a str,
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
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub execution_state: &'a str,
    pub query_text: &'a str,
    pub failure_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct UpdateQueryExecution<'a> {
    pub execution_state: &'a str,
    pub request_turn_id: Option<Uuid>,
    pub response_turn_id: Option<Uuid>,
    pub failure_code: Option<&'a str>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewQueryChunkReference {
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct NewQueryGraphNodeReference {
    pub node_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct NewQueryGraphEdgeReference {
    pub edge_id: Uuid,
    pub rank: i32,
    pub score: f64,
}

pub async fn list_conversations_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<QueryConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryConversationRow>(
        "select
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state,
            created_at,
            updated_at
         from query_conversation
         where library_id = $1
         order by updated_at desc, created_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_conversation_by_id(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Option<QueryConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryConversationRow>(
        "select
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state,
            created_at,
            updated_at
         from query_conversation
         where id = $1",
    )
    .bind(conversation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_conversation(
    postgres: &PgPool,
    input: &NewQueryConversation<'_>,
) -> Result<QueryConversationRow, sqlx::Error> {
    sqlx::query_as::<_, QueryConversationRow>(
        "insert into query_conversation (
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6::query_conversation_state, now(), now())
        returning
            id,
            workspace_id,
            library_id,
            created_by_principal_id,
            title,
            conversation_state::text as conversation_state,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.created_by_principal_id)
    .bind(input.title)
    .bind(input.conversation_state)
    .fetch_one(postgres)
    .await
}

pub async fn list_turns_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryTurnRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryTurnRow>(
        "select
            id,
            conversation_id,
            turn_index,
            turn_kind::text as turn_kind,
            author_principal_id,
            content_text,
            execution_id,
            created_at
         from query_turn
         where conversation_id = $1
         order by turn_index asc",
    )
    .bind(conversation_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_turn_by_id(
    postgres: &PgPool,
    turn_id: Uuid,
) -> Result<Option<QueryTurnRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryTurnRow>(
        "select
            id,
            conversation_id,
            turn_index,
            turn_kind::text as turn_kind,
            author_principal_id,
            content_text,
            execution_id,
            created_at
         from query_turn
         where id = $1",
    )
    .bind(turn_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_turn(
    postgres: &PgPool,
    input: &NewQueryTurn<'_>,
) -> Result<QueryTurnRow, sqlx::Error> {
    sqlx::query_as::<_, QueryTurnRow>(
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
            turn_kind::text as turn_kind,
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
    .await
}

pub async fn list_executions_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRow>(
        "select
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            execution_state::text as execution_state,
            query_text,
            failure_code,
            started_at,
            completed_at
         from query_execution
         where conversation_id = $1
         order by started_at asc",
    )
    .bind(conversation_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_execution_by_id(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRow>(
        "select
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            execution_state::text as execution_state,
            query_text,
            failure_code,
            started_at,
            completed_at
         from query_execution
         where id = $1",
    )
    .bind(execution_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_execution(
    postgres: &PgPool,
    input: &NewQueryExecution<'_>,
) -> Result<QueryExecutionRow, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRow>(
        "insert into query_execution (
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            execution_state,
            query_text,
            failure_code,
            started_at,
            completed_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8::query_execution_state, $9, $10, now(), null)
        returning
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            execution_state::text as execution_state,
            query_text,
            failure_code,
            started_at,
            completed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.binding_id)
    .bind(input.execution_state)
    .bind(input.query_text)
    .bind(input.failure_code)
    .fetch_one(postgres)
    .await
}

pub async fn update_execution(
    postgres: &PgPool,
    execution_id: Uuid,
    input: &UpdateQueryExecution<'_>,
) -> Result<Option<QueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionRow>(
        "update query_execution
         set execution_state = $2::query_execution_state,
             request_turn_id = $3,
             response_turn_id = $4,
             failure_code = $5,
             completed_at = $6
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            binding_id,
            execution_state::text as execution_state,
            query_text,
            failure_code,
            started_at,
            completed_at",
    )
    .bind(execution_id)
    .bind(input.execution_state)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.failure_code)
    .bind(input.completed_at)
    .fetch_optional(postgres)
    .await
}

pub async fn replace_chunk_references(
    postgres: &PgPool,
    execution_id: Uuid,
    references: &[NewQueryChunkReference],
) -> Result<Vec<QueryChunkReferenceRow>, sqlx::Error> {
    sqlx::query("delete from query_chunk_reference where execution_id = $1")
        .bind(execution_id)
        .execute(postgres)
        .await?;

    let mut rows = Vec::with_capacity(references.len());
    for reference in references {
        let row = sqlx::query_as::<_, QueryChunkReferenceRow>(
            "insert into query_chunk_reference (
                execution_id,
                chunk_id,
                rank,
                score
            )
            values ($1, $2, $3, $4)
            returning execution_id, chunk_id, rank, score",
        )
        .bind(execution_id)
        .bind(reference.chunk_id)
        .bind(reference.rank)
        .bind(reference.score)
        .fetch_one(postgres)
        .await?;
        rows.push(row);
    }

    Ok(rows)
}

pub async fn replace_graph_node_references(
    postgres: &PgPool,
    execution_id: Uuid,
    references: &[NewQueryGraphNodeReference],
) -> Result<Vec<QueryGraphNodeReferenceRow>, sqlx::Error> {
    sqlx::query("delete from query_graph_node_reference where execution_id = $1")
        .bind(execution_id)
        .execute(postgres)
        .await?;

    let mut rows = Vec::with_capacity(references.len());
    for reference in references {
        let row = sqlx::query_as::<_, QueryGraphNodeReferenceRow>(
            "insert into query_graph_node_reference (
                execution_id,
                node_id,
                rank,
                score
            )
            values ($1, $2, $3, $4)
            returning execution_id, node_id, rank, score",
        )
        .bind(execution_id)
        .bind(reference.node_id)
        .bind(reference.rank)
        .bind(reference.score)
        .fetch_one(postgres)
        .await?;
        rows.push(row);
    }

    Ok(rows)
}

pub async fn replace_graph_edge_references(
    postgres: &PgPool,
    execution_id: Uuid,
    references: &[NewQueryGraphEdgeReference],
) -> Result<Vec<QueryGraphEdgeReferenceRow>, sqlx::Error> {
    sqlx::query("delete from query_graph_edge_reference where execution_id = $1")
        .bind(execution_id)
        .execute(postgres)
        .await?;

    let mut rows = Vec::with_capacity(references.len());
    for reference in references {
        let row = sqlx::query_as::<_, QueryGraphEdgeReferenceRow>(
            "insert into query_graph_edge_reference (
                execution_id,
                edge_id,
                rank,
                score
            )
            values ($1, $2, $3, $4)
            returning execution_id, edge_id, rank, score",
        )
        .bind(execution_id)
        .bind(reference.edge_id)
        .bind(reference.rank)
        .bind(reference.score)
        .fetch_one(postgres)
        .await?;
        rows.push(row);
    }

    Ok(rows)
}

pub async fn list_chunk_references_by_execution(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Vec<QueryChunkReferenceRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryChunkReferenceRow>(
        "select execution_id, chunk_id, rank, score
         from query_chunk_reference
         where execution_id = $1
         order by rank asc, chunk_id asc",
    )
    .bind(execution_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_graph_node_references_by_execution(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Vec<QueryGraphNodeReferenceRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryGraphNodeReferenceRow>(
        "select execution_id, node_id, rank, score
         from query_graph_node_reference
         where execution_id = $1
         order by rank asc, node_id asc",
    )
    .bind(execution_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_graph_edge_references_by_execution(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Vec<QueryGraphEdgeReferenceRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryGraphEdgeReferenceRow>(
        "select execution_id, edge_id, rank, score
         from query_graph_edge_reference
         where execution_id = $1
         order by rank asc, edge_id asc",
    )
    .bind(execution_id)
    .fetch_all(postgres)
    .await
}
