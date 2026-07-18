use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::agent_runtime::RuntimeLifecycleState,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryConversation, QueryConversationDetail, QueryExecution, QueryTurn, QueryTurnKind,
    },
    infra::{
        knowledge_rows::{KnowledgeBundleChunkReferenceRow, KnowledgeChunkRow},
        repositories::query_repository,
    },
    integrations::llm::ChatMessage,
    interfaces::http::router_support::ApiError,
};

use super::{
    ConversationRuntimeContext, CreateConversationCommand, DeleteConversationCommand,
    ExternalConversationTurn, MAX_EFFECTIVE_QUERY_HISTORY_TURNS, MAX_EFFECTIVE_QUERY_TURN_CHARS,
    MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS, MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS,
    MAX_LIBRARY_CONVERSATIONS, MAX_PROMPT_HISTORY_TURN_CHARS, MAX_PROMPT_HISTORY_TURNS,
    QUERY_CONVERSATION_TITLE_LIMIT, QueryService, RenameConversationCommand,
};

const DENSE_HISTORY_LITERAL_MIN_COUNT: usize = 8;
const MAX_HISTORY_LITERAL_ITEMS: usize = 64;
const MAX_HISTORY_LITERAL_CHARS: usize = 160;
const DENSE_HISTORY_LITERAL_RAW_PROSE_CHARS: usize = 640;
const DENSE_HISTORY_LITERAL_PREFIX: &str = "ir.memory.literals.v1:";
const HISTORY_COMPACT_LITERAL_MEMORY_CONTEXT_PREFIX: &str = "ir.context.compact-literal-memory.v1:";
const PRIOR_GROUNDED_REPLAY_EXECUTION_LIMIT: usize = 2;
const PRIOR_GROUNDED_REPLAY_CHUNKS_PER_EXECUTION: usize = 4;
const PRIOR_GROUNDED_REPLAY_CHUNK_CHARS: usize = 520;
const PRIOR_GROUNDED_REPLAY_TOTAL_CHARS: usize = 3_200;
const PRIOR_GROUNDED_REPLAY_MIN_REMAINING_CHARS: usize = 320;

impl QueryService {
    pub async fn list_conversations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<QueryConversation>, ApiError> {
        let rows = query_repository::list_conversations_by_library(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_conversation_row).collect())
    }

    /// Counts persisted turns for one conversation.
    ///
    /// # Errors
    /// Returns an API error when the count query fails.
    pub async fn count_conversation_turns(
        &self,
        state: &AppState,
        conversation_id: Uuid,
    ) -> Result<i64, ApiError> {
        query_repository::count_turns_by_conversation(&state.persistence.postgres, conversation_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))
    }

    /// Lists the persisted turns for one conversation in chronological order.
    ///
    /// # Errors
    /// Returns an API error when the underlying query fails.
    pub async fn list_turns(
        &self,
        state: &AppState,
        conversation_id: Uuid,
    ) -> Result<Vec<QueryTurn>, ApiError> {
        let rows = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_turn_row).collect())
    }

    /// Loads one turn, scoped to its owning conversation.
    ///
    /// # Errors
    /// Returns a not-found error when the turn is absent or belongs to a
    /// different conversation (never leaks cross-session turn identity).
    pub async fn get_turn(
        &self,
        state: &AppState,
        conversation_id: Uuid,
        turn_id: Uuid,
    ) -> Result<QueryTurn, ApiError> {
        let row = query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .filter(|row| row.conversation_id == conversation_id)
            .ok_or_else(|| ApiError::resource_not_found("query_turn", turn_id))?;
        Ok(map_turn_row(row))
    }

    pub async fn get_conversation(
        &self,
        state: &AppState,
        conversation_id: Uuid,
    ) -> Result<QueryConversationDetail, ApiError> {
        let conversation =
            query_repository::get_conversation_by_id(&state.persistence.postgres, conversation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("conversation", conversation_id))?;
        let turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let executions = query_repository::list_executions_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(QueryConversationDetail {
            conversation: map_conversation_row(conversation),
            turns: turns.into_iter().map(map_turn_row).collect(),
            executions: executions.into_iter().map(map_execution_row).collect(),
        })
    }

    pub async fn create_conversation(
        &self,
        state: &AppState,
        command: CreateConversationCommand,
    ) -> Result<QueryConversation, ApiError> {
        let title =
            command.title.as_deref().map(normalize_explicit_conversation_title).transpose()?;
        let library =
            state.canonical_services.catalog.get_library(state, command.library_id).await?;
        if library.workspace_id != command.workspace_id {
            return Err(ApiError::Conflict(format!(
                "library {} does not belong to workspace {}",
                library.id, command.workspace_id
            )));
        }
        if library.lifecycle_state != CatalogLifecycleState::Active {
            return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
        }
        let row = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: command.created_by_principal_id,
                title: title.as_deref(),
                conversation_state: "active",
                request_surface: command.request_surface.as_str(),
            },
            MAX_LIBRARY_CONVERSATIONS,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_conversation_row(row))
    }

    /// Persists an explicit title after applying the canonical title contract.
    ///
    /// # Errors
    /// Returns a request error for an invalid title or when the guarded row is unavailable.
    pub async fn rename_conversation(
        &self,
        state: &AppState,
        command: RenameConversationCommand,
    ) -> Result<QueryConversation, ApiError> {
        let title = normalize_explicit_conversation_title(&command.title)?;
        let row = query_repository::rename_ui_conversation(
            &state.persistence.postgres,
            command.conversation_id,
            command.actor_principal_id,
            command.allow_manage_all,
            &title,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("query_session", command.conversation_id))?;
        Ok(map_conversation_row(row))
    }

    /// Deletes a UI conversation when ownership, lifecycle, and provenance allow it.
    ///
    /// # Errors
    /// Returns a conflict while work or external replay provenance retains the session.
    pub async fn delete_conversation(
        &self,
        state: &AppState,
        command: DeleteConversationCommand,
    ) -> Result<(), ApiError> {
        let outcome = query_repository::delete_ui_conversation(
            &state.persistence.postgres,
            command.conversation_id,
            command.actor_principal_id,
            command.allow_manage_all,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        match outcome {
            query_repository::DeleteQueryConversationOutcome::Deleted => Ok(()),
            query_repository::DeleteQueryConversationOutcome::NotFoundOrForbidden => {
                Err(ApiError::resource_not_found("query_session", command.conversation_id))
            }
            query_repository::DeleteQueryConversationOutcome::ActiveExecution => {
                Err(ApiError::Conflict("query session has an active execution".to_string()))
            }
            query_repository::DeleteQueryConversationOutcome::RetainedByExternalReplay => {
                Err(ApiError::Conflict(
                    "query session is retained by external replay provenance".to_string(),
                ))
            }
        }
    }

    /// Bounds completed tool-created conversation state stored on the
    /// non-listable `mcp` surface, regardless of whether the external MCP
    /// transport or the in-process UI agent initiated the tool call. The
    /// just-completed conversation is protected so its trace identifiers
    /// remain available to the current caller.
    pub async fn enforce_transient_conversation_retention(
        &self,
        state: &AppState,
        library_id: Uuid,
        protected_conversation_id: Uuid,
    ) -> Result<u64, ApiError> {
        query_repository::prune_mcp_conversation_overflow(
            &state.persistence.postgres,
            library_id,
            MAX_LIBRARY_CONVERSATIONS,
            protected_conversation_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
    }
}

pub(crate) fn map_conversation_row(
    row: query_repository::QueryConversationRow,
) -> QueryConversation {
    QueryConversation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        created_by_principal_id: row.created_by_principal_id,
        title: row.title,
        conversation_state: row.conversation_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

pub(crate) fn map_turn_row(row: query_repository::QueryTurnRow) -> QueryTurn {
    QueryTurn {
        id: row.id,
        conversation_id: row.conversation_id,
        turn_index: row.turn_index,
        turn_kind: row.turn_kind,
        author_principal_id: row.author_principal_id,
        content_text: row.content_text,
        execution_id: row.execution_id,
        created_at: row.created_at,
    }
}

pub(crate) fn map_execution_row(row: query_repository::QueryExecutionRow) -> QueryExecution {
    QueryExecution {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        conversation_id: row.conversation_id,
        context_bundle_id: row.context_bundle_id,
        request_turn_id: row.request_turn_id,
        response_turn_id: row.response_turn_id,
        binding_id: row.binding_id,
        runtime_execution_id: Some(row.runtime_execution_id),
        lifecycle_state: row.runtime_lifecycle_state,
        active_stage: row.runtime_active_stage,
        query_text: row.query_text,
        failure_code: row.failure_code,
        started_at: row.started_at,
        completed_at: row.completed_at,
    }
}

pub(crate) fn normalize_required_text(value: &str, field: &str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field} is required")));
    }
    Ok(normalized.to_string())
}

pub(crate) fn normalize_explicit_conversation_title(value: &str) -> Result<String, ApiError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(ApiError::BadRequest("title is required".to_string()));
    }
    if normalized.chars().count() > QUERY_CONVERSATION_TITLE_LIMIT {
        return Err(ApiError::BadRequest(format!(
            "title must not exceed {QUERY_CONVERSATION_TITLE_LIMIT} characters"
        )));
    }
    Ok(normalized)
}

pub(crate) fn derive_conversation_title(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    let truncated = if collapsed.chars().count() <= QUERY_CONVERSATION_TITLE_LIMIT {
        collapsed
    } else {
        let prefix = collapsed
            .chars()
            .take(QUERY_CONVERSATION_TITLE_LIMIT.saturating_sub(1))
            .collect::<String>();
        format!("{}…", prefix.trim_end())
    };

    Some(truncated)
}

pub(crate) fn should_refresh_conversation_title(current: Option<&str>, candidate: &str) -> bool {
    current.is_none_or(|current| {
        is_weak_conversation_title(current) && !is_weak_conversation_title(candidate)
    })
}

fn is_weak_conversation_title(value: &str) -> bool {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return true;
    }
    let chars = collapsed.chars().count();
    let words = collapsed.split_whitespace().count();
    chars <= 6 || (words <= 1 && chars <= 14)
}

pub(crate) fn build_conversation_runtime_context(
    turns: &[query_repository::QueryTurnRow],
    current_turn_id: Uuid,
) -> ConversationRuntimeContext {
    let current_index = turns
        .iter()
        .position(|turn| turn.id == current_turn_id)
        .unwrap_or_else(|| turns.len().saturating_sub(1));
    let relevant_turns = &turns[..=current_index.min(turns.len().saturating_sub(1))];
    let views = relevant_turns
        .iter()
        .map(|turn| RuntimeContextTurn {
            turn_kind: turn.turn_kind.clone(),
            content_text: turn.content_text.as_str(),
        })
        .collect::<Vec<_>>();
    build_conversation_runtime_context_from_views(&views)
}

pub(crate) fn build_conversation_runtime_context_with_external_history(
    turns: &[query_repository::QueryTurnRow],
    current_turn_id: Uuid,
    external_prior_turns: &[ExternalConversationTurn],
) -> ConversationRuntimeContext {
    let current_index = turns
        .iter()
        .position(|turn| turn.id == current_turn_id)
        .unwrap_or_else(|| turns.len().saturating_sub(1));
    let relevant_turns = &turns[..=current_index.min(turns.len().saturating_sub(1))];
    let mut views =
        Vec::with_capacity(relevant_turns.len().saturating_add(external_prior_turns.len()));
    for turn in relevant_turns.iter().take(relevant_turns.len().saturating_sub(1)) {
        views.push(RuntimeContextTurn {
            turn_kind: turn.turn_kind.clone(),
            content_text: turn.content_text.as_str(),
        });
    }
    for turn in external_prior_turns {
        views.push(RuntimeContextTurn {
            turn_kind: turn.turn_kind.clone(),
            content_text: turn.content_text.as_str(),
        });
    }
    if let Some(current_turn) = relevant_turns.last() {
        views.push(RuntimeContextTurn {
            turn_kind: current_turn.turn_kind.clone(),
            content_text: current_turn.content_text.as_str(),
        });
    }
    build_conversation_runtime_context_from_views(&views)
}

pub(crate) fn should_replay_prior_grounded_answer_context(
    context: &ConversationRuntimeContext,
) -> bool {
    context.has_prior_conversation && !context.prompt_history_messages.is_empty()
}

pub(crate) async fn load_prior_grounded_answer_context_messages(
    state: &AppState,
    conversation_id: Uuid,
    library_id: Uuid,
) -> Result<Vec<ChatMessage>, ApiError> {
    let executions = query_repository::list_executions_by_conversation(
        &state.persistence.postgres,
        conversation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let selected = select_prior_grounded_answer_replay_executions(
        executions,
        library_id,
        PRIOR_GROUNDED_REPLAY_EXECUTION_LIMIT,
    );

    let mut messages = Vec::new();
    let mut seen_chunk_ids = HashSet::new();
    let mut remaining_chars = PRIOR_GROUNDED_REPLAY_TOTAL_CHARS;
    for execution in selected {
        if remaining_chars < PRIOR_GROUNDED_REPLAY_MIN_REMAINING_CHARS {
            break;
        }
        let bundle = match state
            .context_store
            .get_bundle_reference_set_by_query_execution(execution.id)
            .await
        {
            Ok(Some(bundle)) => bundle,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    execution_id = %execution.id,
                    "prior grounded-answer context replay skipped after context-bundle lookup failure"
                );
                continue;
            }
        };
        let candidate_refs = bundle
            .chunk_references
            .iter()
            .filter(|reference| !seen_chunk_ids.contains(&reference.chunk_id))
            .take(PRIOR_GROUNDED_REPLAY_CHUNKS_PER_EXECUTION)
            .cloned()
            .collect::<Vec<_>>();
        if candidate_refs.is_empty() {
            continue;
        }
        let chunk_ids =
            candidate_refs.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
        let chunk_rows = match state.document_store.list_chunks_by_ids(&chunk_ids).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    execution_id = %execution.id,
                    chunk_count = chunk_ids.len(),
                    "prior grounded-answer context replay skipped after chunk hydration failure"
                );
                continue;
            }
        };
        if let Some(replay) = build_prior_grounded_answer_context_messages(
            library_id,
            execution.id,
            &execution.query_text,
            &candidate_refs,
            &chunk_rows,
            remaining_chars,
        ) {
            for chunk_id in replay.chunk_ids {
                seen_chunk_ids.insert(chunk_id);
            }
            remaining_chars = remaining_chars.saturating_sub(replay.char_count);
            messages.extend(replay.messages);
        }
    }

    Ok(messages)
}

pub(crate) fn select_prior_grounded_answer_replay_executions(
    executions: impl IntoIterator<Item = query_repository::QueryExecutionRow>,
    library_id: Uuid,
    limit: usize,
) -> Vec<query_repository::QueryExecutionRow> {
    executions
        .into_iter()
        .filter(|execution| {
            execution.library_id == library_id
                && execution.runtime_lifecycle_state == RuntimeLifecycleState::Completed
                && execution.failure_code.is_none()
        })
        .take(limit)
        .collect()
}

#[derive(Debug)]
pub(crate) struct PriorGroundedAnswerReplay {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) chunk_ids: Vec<Uuid>,
    pub(crate) char_count: usize,
}

pub(crate) fn build_prior_grounded_answer_context_messages(
    library_id: Uuid,
    execution_id: Uuid,
    query_text: &str,
    chunk_references: &[KnowledgeBundleChunkReferenceRow],
    chunk_rows: &[KnowledgeChunkRow],
    max_chars: usize,
) -> Option<PriorGroundedAnswerReplay> {
    let rows_by_id = chunk_rows
        .iter()
        .map(|row| (row.chunk_id, row))
        .collect::<HashMap<Uuid, &KnowledgeChunkRow>>();
    let mut chunk_ids = Vec::new();
    let mut lines =
        Vec::with_capacity(PRIOR_GROUNDED_REPLAY_CHUNKS_PER_EXECUTION.saturating_add(5));
    lines.push("Earlier grounded answer evidence from this conversation.".to_string());
    lines.push(
        "Use this only to preserve follow-up continuity; call tools again when the current question needs fresh retrieval."
            .to_string(),
    );
    lines.push(format!("executionId: {execution_id}"));
    lines.push(format!("query: {}", compact_conversation_turn_text(query_text, 360)));
    lines.push("chunks:".to_string());

    let mut sorted_references = chunk_references.iter().collect::<Vec<_>>();
    sorted_references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal))
    });

    for reference in sorted_references {
        if chunk_ids.len() >= PRIOR_GROUNDED_REPLAY_CHUNKS_PER_EXECUTION {
            break;
        }
        let Some(row) = rows_by_id.get(&reference.chunk_id).copied() else {
            continue;
        };
        if row.library_id != library_id {
            continue;
        }
        let section = render_chunk_section_label(row);
        let snippet =
            compact_conversation_turn_text(&row.content_text, PRIOR_GROUNDED_REPLAY_CHUNK_CHARS);
        if snippet.is_empty() {
            continue;
        }
        lines.push(format!(
            "- chunkId: {}; documentId: {}; revisionId: {}; rank: {}; score: {:.4}; section: {}",
            row.chunk_id,
            row.document_id,
            row.revision_id,
            reference.rank,
            reference.score,
            section
        ));
        lines.push(format!("  snippet: {snippet}"));
        chunk_ids.push(row.chunk_id);
    }
    if chunk_ids.is_empty() {
        return None;
    }

    let content = compact_conversation_turn_text(&lines.join("\n"), max_chars);
    if content.is_empty() {
        return None;
    }
    let messages = vec![ChatMessage::system(content.clone())];

    Some(PriorGroundedAnswerReplay { messages, chunk_ids, char_count: content.chars().count() })
}

fn render_chunk_section_label(row: &KnowledgeChunkRow) -> String {
    let section = if row.heading_trail.is_empty() {
        row.section_path.join(" > ")
    } else {
        row.heading_trail.join(" > ")
    };
    if section.trim().is_empty() {
        "unsectioned".to_string()
    } else {
        compact_conversation_turn_text(&section, 240)
    }
}

#[derive(Debug, Clone)]
struct RuntimeContextTurn<'a> {
    turn_kind: QueryTurnKind,
    content_text: &'a str,
}

fn build_conversation_runtime_context_from_views(
    turns: &[RuntimeContextTurn<'_>],
) -> ConversationRuntimeContext {
    if turns.is_empty() {
        return ConversationRuntimeContext {
            current_question_text: String::new(),
            has_prior_conversation: false,
            query_compiler_history: Vec::new(),
            prompt_history_text: None,
            prompt_history_messages: Vec::new(),
            grounded_answer_tool_history: Vec::new(),
        };
    }
    let current_turn = turns.last();
    let current_text = current_turn
        .map(|turn| turn.content_text.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let previous_turns = turns[..turns.len().saturating_sub(1)].iter().collect::<Vec<_>>();
    let has_prior_conversation = previous_turns
        .iter()
        .any(|turn| matches!(turn.turn_kind, QueryTurnKind::User | QueryTurnKind::Assistant));
    let prompt_history_text = render_turn_history(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );
    let query_compiler_history = render_external_turn_history(
        &previous_turns,
        MAX_EFFECTIVE_QUERY_HISTORY_TURNS,
        MAX_EFFECTIVE_QUERY_TURN_CHARS,
    );
    let prompt_history_messages = render_prompt_history_messages(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );
    let grounded_answer_tool_history = render_external_turn_history(
        &previous_turns,
        MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS,
        MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS,
    );

    ConversationRuntimeContext {
        current_question_text: current_text,
        has_prior_conversation,
        query_compiler_history,
        prompt_history_text,
        prompt_history_messages,
        grounded_answer_tool_history,
    }
}

fn extract_code_literals_from_text(value: &str) -> Vec<String> {
    let mut literals = extract_backtick_literals_from_text(value);
    dedup_preserve_order(&mut literals);
    literals
}

fn extract_backtick_literals_from_text(value: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut search_from = 0;
    while let Some(start) = value[search_from..].find('`') {
        let abs_start = search_from + start + 1;
        if abs_start >= value.len() {
            break;
        }
        if let Some(end) = value[abs_start..].find('`') {
            let term = &value[abs_start..abs_start + end];
            let char_count = term.chars().count();
            if char_count > 1 && char_count <= MAX_HISTORY_LITERAL_CHARS && !term.contains('\n') {
                literals.push(term.to_string());
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }
    literals
}

fn render_turn_history(
    turns: &[&RuntimeContextTurn<'_>],
    limit: usize,
    max_chars_per_turn: usize,
) -> Option<String> {
    let selected = turns
        .iter()
        .rev()
        .filter_map(|turn| {
            if matches!(turn.turn_kind, QueryTurnKind::System | QueryTurnKind::Tool) {
                return None;
            }
            let text =
                compact_history_turn_text(&turn.turn_kind, turn.content_text, max_chars_per_turn);
            (!text.is_empty())
                .then(|| format!("{}: {}", conversation_turn_speaker(&turn.turn_kind), text))
        })
        .take(limit)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        None
    } else {
        Some(selected.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}

fn render_prompt_history_messages(
    previous_turns: &[&RuntimeContextTurn<'_>],
    limit: usize,
    max_chars_per_turn: usize,
) -> Vec<ChatMessage> {
    let mut selected = previous_turns
        .iter()
        .rev()
        .filter_map(|turn| {
            if matches!(turn.turn_kind, QueryTurnKind::System | QueryTurnKind::Tool) {
                return None;
            }
            let text =
                compact_history_turn_text(&turn.turn_kind, turn.content_text, max_chars_per_turn);
            (!text.is_empty()).then_some((*turn, text))
        })
        .take(limit)
        .collect::<Vec<_>>();
    selected.reverse();
    selected
        .into_iter()
        .filter_map(|(turn, text)| match &turn.turn_kind {
            QueryTurnKind::User => Some(ChatMessage::user(text)),
            QueryTurnKind::Assistant => Some(assistant_history_message(text)),
            QueryTurnKind::System | QueryTurnKind::Tool => None,
        })
        .collect()
}

fn assistant_history_message(text: String) -> ChatMessage {
    if is_compact_literal_memory(&text) {
        return ChatMessage::system(format!(
            "{HISTORY_COMPACT_LITERAL_MEMORY_CONTEXT_PREFIX}\n{text}"
        ));
    }
    ChatMessage::assistant_text(text)
}

fn is_compact_literal_memory(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with(DENSE_HISTORY_LITERAL_PREFIX))
}

fn render_external_turn_history(
    previous_turns: &[&RuntimeContextTurn<'_>],
    limit: usize,
    max_chars_per_turn: usize,
) -> Vec<ExternalConversationTurn> {
    let mut selected = previous_turns
        .iter()
        .rev()
        .filter_map(|turn| match &turn.turn_kind {
            QueryTurnKind::User | QueryTurnKind::Assistant => {
                let text = compact_history_turn_text(
                    &turn.turn_kind,
                    turn.content_text,
                    max_chars_per_turn,
                );
                (!text.is_empty()).then(|| ExternalConversationTurn {
                    turn_kind: turn.turn_kind.clone(),
                    content_text: text,
                })
            }
            QueryTurnKind::System | QueryTurnKind::Tool => None,
        })
        .take(limit)
        .collect::<Vec<_>>();
    selected.reverse();
    selected
}

fn compact_history_turn_text(turn_kind: &QueryTurnKind, value: &str, max_chars: usize) -> String {
    if matches!(turn_kind, QueryTurnKind::Assistant) {
        return compact_assistant_history_text(value, max_chars);
    }
    compact_conversation_turn_text(value, max_chars)
}

fn compact_assistant_history_text(value: &str, max_chars: usize) -> String {
    let literals = extract_code_literals_from_text(value);
    if literals.len() < DENSE_HISTORY_LITERAL_MIN_COUNT {
        return compact_conversation_turn_text(value, max_chars);
    }

    let literal_line = compact_conversation_turn_text(
        &format!(
            "{DENSE_HISTORY_LITERAL_PREFIX} {}",
            literals
                .into_iter()
                .take(MAX_HISTORY_LITERAL_ITEMS)
                .map(|literal| format!("`{literal}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        max_chars,
    );
    let used_chars = literal_line.chars().count();
    let raw_budget = max_chars
        .saturating_sub(used_chars)
        .saturating_sub(1)
        .min(DENSE_HISTORY_LITERAL_RAW_PROSE_CHARS);
    if raw_budget < 80 {
        return literal_line;
    }
    let raw = compact_conversation_turn_text(value, raw_budget);
    if raw.is_empty() { literal_line } else { format!("{literal_line}\n{raw}") }
}

fn compact_conversation_turn_text(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cutoff =
        collapsed.char_indices().nth(max_chars).map_or(collapsed.len(), |(index, _)| index);
    format!("{}…", collapsed[..cutoff].trim_end())
}

fn dedup_preserve_order(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.to_lowercase()));
}

fn conversation_turn_speaker(turn_kind: &QueryTurnKind) -> &'static str {
    match turn_kind {
        QueryTurnKind::Assistant => "Assistant",
        _ => "User",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(turn_kind: QueryTurnKind, content_text: &str) -> RuntimeContextTurn<'_> {
        RuntimeContextTurn { turn_kind, content_text }
    }

    #[test]
    fn current_question_stays_verbatim_while_compiler_history_is_bounded_separately() {
        let turns = [
            turn(QueryTurnKind::User, "prior turn zero"),
            turn(QueryTurnKind::Assistant, "prior turn one"),
            turn(QueryTurnKind::User, "prior turn two"),
            turn(QueryTurnKind::Assistant, "prior turn three"),
            turn(QueryTurnKind::User, "prior turn four"),
            turn(QueryTurnKind::User, "it"),
        ];

        let context = build_conversation_runtime_context_from_views(&turns);

        assert_eq!(context.current_question_text, "it");
        assert!(context.has_prior_conversation);
        assert_eq!(context.query_compiler_history.len(), MAX_EFFECTIVE_QUERY_HISTORY_TURNS);
        assert!(
            context
                .query_compiler_history
                .iter()
                .all(|turn| !turn.content_text.contains("prior turn zero"))
        );
        assert!(
            context
                .query_compiler_history
                .iter()
                .all(|turn| !turn.content_text.contains("question: it"))
        );
    }

    #[test]
    fn history_availability_does_not_depend_on_question_length_or_letter_case() {
        for question in ["x", "ALLCAPS", "a deliberately longer independent current question"] {
            let turns = [
                turn(QueryTurnKind::User, "first prior turn"),
                turn(QueryTurnKind::Assistant, "second prior turn"),
                turn(QueryTurnKind::User, question),
            ];

            let context = build_conversation_runtime_context_from_views(&turns);

            assert!(context.has_prior_conversation);
            assert_eq!(context.query_compiler_history.len(), 2);
            assert_eq!(context.prompt_history_messages.len(), 2);
            assert_eq!(context.grounded_answer_tool_history.len(), 2);
            assert!(should_replay_prior_grounded_answer_context(&context));
        }
    }

    #[test]
    fn compiler_history_contains_only_bounded_user_and_assistant_turns() {
        let turns = [
            turn(QueryTurnKind::System, "private system state"),
            turn(QueryTurnKind::Tool, "private tool state"),
            turn(QueryTurnKind::User, "prior question"),
            turn(QueryTurnKind::Assistant, "prior answer"),
            turn(QueryTurnKind::User, "current question"),
        ];

        let context = build_conversation_runtime_context_from_views(&turns);
        assert_eq!(context.query_compiler_history.len(), 2);
        assert_eq!(context.query_compiler_history[0].turn_kind, QueryTurnKind::User);
        assert_eq!(context.query_compiler_history[0].content_text, "prior question");
        assert_eq!(context.query_compiler_history[1].turn_kind, QueryTurnKind::Assistant);
        assert_eq!(context.query_compiler_history[1].content_text, "prior answer");
        assert!(
            context
                .query_compiler_history
                .iter()
                .all(|turn| !turn.content_text.contains("private"))
        );
    }

    #[test]
    fn compiler_history_preserves_multiline_assistant_memory_as_one_typed_turn() {
        let dense = "`A_1` `A_2` `A_3` `A_4` `A_5` `A_6` `A_7` `A_8`\nstatus: retained";
        let turns =
            [turn(QueryTurnKind::Assistant, dense), turn(QueryTurnKind::User, "current question")];

        let context = build_conversation_runtime_context_from_views(&turns);

        assert_eq!(context.query_compiler_history.len(), 1);
        let history = &context.query_compiler_history[0];
        assert_eq!(history.turn_kind, QueryTurnKind::Assistant);
        assert!(history.content_text.contains('\n'));
        assert!(history.content_text.contains("status: retained"));
    }
}
