use super::*;

pub async fn list_turns_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<Vec<QueryTurnRow>, sqlx::Error> {
    let mut turns = sqlx::query_as::<_, QueryTurnRowRecord>(
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
         order by created_at desc, turn_index desc
         limit 200",
    )
    .bind(conversation_id)
    .fetch_all(postgres)
    .await?
    .into_iter()
    .map(map_query_turn_row)
    .collect::<Result<Vec<_>, _>>()?;
    turns.reverse();
    Ok(turns)
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

/// Counts the persisted turns for one conversation. Used by the session list
/// endpoint so `turnCount` reflects reality instead of the hard-coded `0`
/// the flat `/query/sessions` listing shipped with previously.
pub async fn count_turns_by_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from query_turn where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(postgres)
    .await
}

/// Removes a request that never reached either a paid execution or a cache
/// replay. This is the rollback surface for pre-execution coordination errors
/// (for example a content projection changing while a fill lock is awaited).
/// The negative execution/audit predicates make a late successful owner win:
/// cleanup becomes a no-op instead of deleting accepted conversation history.
pub async fn delete_unexecuted_request_turn(
    postgres: &PgPool,
    conversation_id: Uuid,
    request_turn_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query(
        "delete from query_turn as request_turn
         where request_turn.id = $1
           and request_turn.conversation_id = $2
           and request_turn.turn_kind = 'user'
           and request_turn.execution_id is null
           and not exists (
               select 1
               from query_execution execution
               where execution.request_turn_id = request_turn.id
           )
           and not exists (
               select 1
               from query_execution_replay replay
               where replay.request_turn_id = request_turn.id
           )",
    )
    .bind(request_turn_id)
    .bind(conversation_id)
    .execute(postgres)
    .await?;
    Ok(deleted.rows_affected() == 1)
}

pub async fn create_turn<'borrow, 'data>(
    postgres: &'borrow PgPool,
    input: &'borrow NewQueryTurn<'data>,
) -> Result<QueryTurnRow, sqlx::Error>
where
    'data: 'borrow,
{
    let mut connection = postgres.acquire().await?;
    create_turn_in_connection(&mut connection, input).await
}

pub(crate) async fn create_turn_in_connection<'borrow, 'data>(
    connection: &'borrow mut PgConnection,
    input: &'borrow NewQueryTurn<'data>,
) -> Result<QueryTurnRow, sqlx::Error>
where
    'data: 'borrow,
{
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
    .fetch_one(connection)
    .await?;
    map_query_turn_row(row)
}
