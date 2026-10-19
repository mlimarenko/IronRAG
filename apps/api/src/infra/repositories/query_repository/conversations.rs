use super::*;

pub async fn list_conversations_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<QueryConversationRow>, sqlx::Error> {
    // UI-only listing: MCP-born rows are transient execution state and must
    // not surface in the web assistant session list. Their durable audit
    // trail lives independently in `audit_event` / `audit_event_subject`.
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
    acquire_conversation_retention_lock(&mut transaction, input.library_id, input.request_surface)
        .await?;
    delete_conversation_overflow(
        &mut transaction,
        input.library_id,
        input.request_surface,
        max_library_conversations,
        1,
        None,
    )
    .await?;

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

/// Re-applies the MCP retention cap after a tool execution reaches a terminal
/// state. This closes the burst window where several fresh conversations can
/// all be active during creation and only become evictable afterwards.
///
/// The current response's conversation is protected so the caller's returned
/// trace identifiers remain resolvable. Completed conversations become
/// prunable only after a grace period from their last runtime completion:
/// a concurrent caller protects only its own conversation, and reaping a
/// sibling's just-completed child conversation would cascade away the
/// `query_execution` row its parent still reads while attaching grounding.
/// Stored `audit_event` and `audit_event_subject` rows are independent
/// durable records without foreign keys back to query storage.
pub async fn prune_mcp_conversation_overflow(
    postgres: &PgPool,
    library_id: Uuid,
    max_library_conversations: usize,
    protected_conversation_id: Uuid,
) -> Result<u64, sqlx::Error> {
    // Serialize concurrent pruning for this (library, surface) and enforce
    // retention under a snapshot taken after the lock is held. Reuses the same
    // lock-then-delete helpers as `create_conversation`: acquiring the advisory
    // lock in its own statement means a caller that blocked on it re-reads the
    // conversation count afterwards, instead of enforcing retention against a
    // stale count and deleting past the limit.
    let mut transaction = postgres.begin().await?;
    acquire_conversation_retention_lock(&mut transaction, library_id, MCP_REQUEST_SURFACE).await?;
    let deleted = delete_conversation_overflow(
        &mut transaction,
        library_id,
        MCP_REQUEST_SURFACE,
        max_library_conversations,
        0,
        Some(protected_conversation_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(deleted)
}

async fn acquire_conversation_retention_lock(
    connection: &mut PgConnection,
    library_id: Uuid,
    request_surface: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(CONVERSATION_RETENTION_LOCK_SQL)
        .bind(library_id)
        .bind(request_surface)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn delete_conversation_overflow(
    connection: &mut PgConnection,
    library_id: Uuid,
    request_surface: &str,
    max_library_conversations: usize,
    reserved_rows: i64,
    protected_conversation_id: Option<Uuid>,
) -> Result<u64, sqlx::Error> {
    let existing_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from query_conversation
         where library_id = $1
           and request_surface = $2::surface_kind",
    )
    .bind(library_id)
    .bind(request_surface)
    .fetch_one(&mut *connection)
    .await?;

    let max_library_conversations =
        i64::try_from(max_library_conversations.max(1)).unwrap_or(i64::MAX);
    let overflow_count = existing_count
        .saturating_add(reserved_rows.max(0))
        .saturating_sub(max_library_conversations);

    if overflow_count <= 0 {
        return Ok(0);
    }

    let delete_sql = if request_surface == MCP_REQUEST_SURFACE {
        MCP_CONVERSATION_OVERFLOW_DELETE_SQL
    } else {
        AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL
    };
    let result = sqlx::query(delete_sql)
        .bind(library_id)
        .bind(request_surface)
        .bind(overflow_count)
        .bind(protected_conversation_id)
        .execute(&mut *connection)
        .await?;
    Ok(result.rows_affected())
}

/// Sets the automatically-derived title only while the conversation is untitled.
///
/// # Errors
/// Returns a repository error when the conversation cannot be loaded or updated.
pub async fn initialize_conversation_title(
    postgres: &PgPool,
    conversation_id: Uuid,
    title: &str,
) -> Result<QueryConversationRow, sqlx::Error> {
    let row = sqlx::query_as::<_, QueryConversationRowRecord>(
        "with initialized as (
            update query_conversation
            set title = $2,
                updated_at = now()
            where id = $1
              and title is null
            returning id
         )
         select
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
    .bind(title)
    .fetch_one(postgres)
    .await?;
    map_query_conversation_row(row)
}

/// Persists an explicit title for an owned or manager-authorized UI conversation.
///
/// # Errors
/// Returns a repository error when the update cannot be executed.
pub async fn rename_ui_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
    actor_principal_id: Uuid,
    allow_manage_all: bool,
    title: &str,
) -> Result<Option<QueryConversationRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryConversationRowRecord>(
        "update query_conversation
         set title = $4,
             updated_at = now()
         where id = $1
           and request_surface = 'ui'
           and (created_by_principal_id = $2 or $3::boolean)
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
    .bind(actor_principal_id)
    .bind(allow_manage_all)
    .bind(title)
    .fetch_optional(postgres)
    .await?
    .map(map_query_conversation_row)
    .transpose()
}

/// Atomically deletes a UI conversation when lifecycle and provenance guards allow it.
///
/// # Errors
/// Returns a repository error when locking, guard evaluation, or deletion fails.
pub async fn delete_ui_conversation(
    postgres: &PgPool,
    conversation_id: Uuid,
    actor_principal_id: Uuid,
    allow_manage_all: bool,
) -> Result<DeleteQueryConversationOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from query_conversation
         where id = $1
           and request_surface = 'ui'
           and (created_by_principal_id = $2 or $3::boolean)
         for update",
    )
    .bind(conversation_id)
    .bind(actor_principal_id)
    .bind(allow_manage_all)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked.is_none() {
        transaction.commit().await?;
        return Ok(DeleteQueryConversationOutcome::NotFoundOrForbidden);
    }

    // Lock source executions before inspecting replay references. A concurrent
    // replay insert must acquire a foreign-key key-share lock and therefore
    // cannot appear between this check and the conversation delete.
    let _locked_execution_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from query_execution
         where conversation_id = $1
         for update",
    )
    .bind(conversation_id)
    .fetch_all(&mut *transaction)
    .await?;

    let has_active_execution = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from query_execution execution
            join runtime_execution runtime
              on runtime.id = execution.runtime_execution_id
             and runtime.owner_kind = 'query_execution'
             and runtime.owner_id = execution.id
            where execution.conversation_id = $1
              and runtime.lifecycle_state in ('accepted', 'running')
         )",
    )
    .bind(conversation_id)
    .fetch_one(&mut *transaction)
    .await?;
    if has_active_execution {
        transaction.commit().await?;
        return Ok(DeleteQueryConversationOutcome::ActiveExecution);
    }

    let has_external_replay = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from query_execution source_execution
            join query_execution_replay replay
              on replay.source_execution_id = source_execution.id
            where source_execution.conversation_id = $1
              and replay.conversation_id <> $1
         )",
    )
    .bind(conversation_id)
    .fetch_one(&mut *transaction)
    .await?;
    if has_external_replay {
        transaction.commit().await?;
        return Ok(DeleteQueryConversationOutcome::RetainedByExternalReplay);
    }

    let deleted = sqlx::query("delete from query_conversation where id = $1")
        .bind(conversation_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    if deleted.rows_affected() == 1 {
        Ok(DeleteQueryConversationOutcome::Deleted)
    } else {
        Ok(DeleteQueryConversationOutcome::NotFoundOrForbidden)
    }
}
