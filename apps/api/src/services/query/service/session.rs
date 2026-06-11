use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::agent_runtime::RuntimeLifecycleState,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryConversation, QueryConversationDetail, QueryExecution, QueryTurn, QueryTurnKind,
    },
    domains::query_ir::literal_text_is_identifier_shaped,
    infra::{
        arangodb::{
            context_store::KnowledgeBundleChunkReferenceRow, document_store::KnowledgeChunkRow,
        },
        repositories::query_repository,
    },
    integrations::llm::ChatMessage,
    interfaces::http::router_support::ApiError,
    services::query::{
        effective_query::{EFFECTIVE_QUERY_QUESTION_PREFIX, EFFECTIVE_QUERY_SCOPE_PREFIX},
        planner::strip_leading_question_marker,
        text_match::{near_token_match, normalized_alnum_tokens},
    },
};

use super::{
    ConversationRuntimeContext, CreateConversationCommand, ExternalConversationTurn,
    MAX_EFFECTIVE_QUERY_HISTORY_TURNS, MAX_EFFECTIVE_QUERY_TURN_CHARS,
    MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS, MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS,
    MAX_LIBRARY_CONVERSATIONS, MAX_PROMPT_HISTORY_TURN_CHARS, MAX_PROMPT_HISTORY_TURNS,
    QUERY_CONVERSATION_TITLE_LIMIT, QueryService,
};

const MAX_COREFERENCE_ENTITIES: usize = 64;
const MAX_QUERY_CONTEXT_ENTITY_HINTS: usize = 48;
const MAX_EFFECTIVE_QUERY_ENTITY_SCOPE_ITEMS: usize = 64;
const DENSE_HISTORY_LITERAL_MIN_COUNT: usize = 8;
const MAX_HISTORY_LITERAL_ITEMS: usize = 64;
const MAX_HISTORY_LITERAL_CHARS: usize = 160;
const DENSE_HISTORY_LITERAL_RAW_PROSE_CHARS: usize = 640;
const DENSE_HISTORY_LITERAL_PREFIX: &str = "literals:";
const HISTORY_LITERAL_ANCHOR_PREFIX: &str = "literal anchors:";
const MAX_HISTORY_LITERAL_ANCHOR_ITEMS: usize = 64;
const MAX_HISTORY_LITERAL_ANCHOR_CHARS: usize = 1_800;
const HISTORY_OVERLAP_FOLLOW_UP_MAX_TOKENS: usize = 8;
const DENSE_LITERAL_HISTORY_FOLLOW_UP_MAX_TOKENS: usize = 24;
const CONTEXT_DEPENDENT_FOLLOW_UP_MAX_TOKENS: usize = 6;
const HISTORY_OVERLAP_FOLLOW_UP_MIN_MATCHES: usize = 2;
const HISTORY_OVERLAP_FOLLOW_UP_LOOKBACK_TURNS: usize = 6;
const HISTORY_SUBJECT_TOKEN_CAP: usize = 16;
const HISTORY_SUBJECT_LOOKBACK_TURNS: usize = 8;
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
        let title = normalize_optional_text(command.title.as_deref());
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

pub(crate) fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

pub(crate) fn normalize_required_text(value: &str, field: &str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field} is required")));
    }
    Ok(normalized.to_string())
}

pub(crate) fn derive_conversation_title(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    let truncated = if collapsed.chars().count() <= QUERY_CONVERSATION_TITLE_LIMIT {
        collapsed
    } else {
        let cutoff = collapsed
            .char_indices()
            .nth(QUERY_CONVERSATION_TITLE_LIMIT)
            .map_or(collapsed.len(), |(index, _)| index);
        format!("{}…", collapsed[..cutoff].trim_end())
    };

    Some(truncated)
}

pub(crate) fn should_refresh_conversation_title(current: Option<&str>, candidate: &str) -> bool {
    current.map_or(true, |current| {
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
    build_conversation_runtime_context_from_views(&views, false)
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
    build_conversation_runtime_context_from_views(&views, !external_prior_turns.is_empty())
}

pub(crate) fn should_replay_prior_grounded_answer_context(
    context: &ConversationRuntimeContext,
) -> bool {
    context.contextual_follow_up && !context.prompt_history_messages.is_empty()
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
            .arango_context_store
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
        let chunk_rows = match state.arango_document_store.list_chunks_by_ids(&chunk_ids).await {
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
    let mut lines = vec![
        "Earlier grounded answer evidence from this conversation.".to_string(),
        "Use this only to preserve follow-up continuity; call tools again when the current question needs fresh retrieval.".to_string(),
        format!("executionId: {execution_id}"),
        format!("query: {}", compact_conversation_turn_text(query_text, 360)),
        "chunks:".to_string(),
    ];

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
    force_context_scope: bool,
) -> ConversationRuntimeContext {
    if turns.is_empty() {
        return ConversationRuntimeContext {
            effective_query_text: String::new(),
            contextual_follow_up: false,
            query_planning_history_text: None,
            prompt_history_text: None,
            prompt_history_messages: Vec::new(),
            grounded_answer_tool_history: Vec::new(),
            coreference_entities: Vec::new(),
        };
    }
    let current_turn = turns.last();
    let current_text = current_turn
        .map(|turn| turn.content_text.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let previous_turns = turns[..turns.len().saturating_sub(1)].iter().collect::<Vec<_>>();
    let has_previous_assistant_turn =
        previous_turns.iter().any(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant));
    let follow_up_focus_tokens = effective_query_focus_tokens(&current_text);
    let subject_tokens = conversation_subject_tokens(&previous_turns);
    let is_follow_up = has_previous_assistant_turn && is_context_dependent_follow_up(&current_text)
        || is_history_overlapping_follow_up(
            &previous_turns,
            &current_text,
            &follow_up_focus_tokens,
        );
    let should_scope_with_history =
        is_follow_up || force_context_scope && !turns[..turns.len().saturating_sub(1)].is_empty();
    let prompt_history_text = render_turn_history(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );
    let query_planning_history_text = render_user_turn_history(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );
    let pinned_literal_anchor_scope = pinned_previous_assistant_literal_anchor_scope(
        &previous_turns,
        &current_text,
        should_scope_with_history,
    );
    let mut prompt_history_messages = render_prompt_history_messages(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );
    if let Some(anchor_scope) = &pinned_literal_anchor_scope {
        prompt_history_messages.insert(0, pinned_literal_anchor_message(anchor_scope.clone()));
    }
    let mut grounded_answer_tool_history = render_grounded_answer_tool_history(
        &previous_turns,
        MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS,
        MAX_GROUNDED_ANSWER_TOOL_HISTORY_CHARS,
    );
    if let Some(anchor_scope) = &pinned_literal_anchor_scope {
        grounded_answer_tool_history.insert(
            0,
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: anchor_scope.clone(),
            },
        );
    }

    let coreference_entities = if should_scope_with_history {
        previous_assistant_context_source(&previous_turns, &follow_up_focus_tokens, &subject_tokens)
            .map(|source| extract_entities_from_previous_answer(&source))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let effective_query_text = if should_scope_with_history {
        render_effective_query_text(&previous_turns, &current_text).unwrap_or(current_text)
    } else {
        current_text
    };

    ConversationRuntimeContext {
        effective_query_text,
        contextual_follow_up: should_scope_with_history,
        query_planning_history_text,
        prompt_history_text,
        prompt_history_messages,
        grounded_answer_tool_history,
        coreference_entities,
    }
}

pub(crate) fn enrich_query_with_coreference_entities(query: &str, entities: &[String]) -> String {
    if entities.is_empty() {
        return query.to_string();
    }
    // Only add entities that are not already mentioned in the query
    let query_lower = query.to_lowercase();
    let novel: Vec<&str> = entities
        .iter()
        .filter(|entity| !query_lower.contains(&entity.to_lowercase()))
        .map(String::as_str)
        .take(MAX_QUERY_CONTEXT_ENTITY_HINTS)
        .collect();
    if novel.is_empty() {
        return query.to_string();
    }
    format!("{query} (context entities: {})", novel.join(", "))
}

fn extract_entities_from_previous_answer(answer: &str) -> Vec<String> {
    let mut entities = Vec::new();

    // Bare capitalised tokens fed to the coreference resolver; false
    // positives (spurious "Both" etc.) only cause a missed follow-up-
    // sharpening, never a wrong answer.
    for word in answer.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
        if clean.chars().count() >= 4 && clean.chars().next().is_some_and(char::is_uppercase) {
            entities.push(clean.to_string());
        }
    }

    dedup_preserve_order(&mut entities);
    entities.truncate(MAX_COREFERENCE_ENTITIES);
    entities
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

fn previous_assistant_literal_anchor_scope(
    previous_turns: &[&RuntimeContextTurn<'_>],
    current_text: &str,
    focus_tokens: &[String],
    subject_tokens: &[String],
) -> Option<String> {
    if !extract_retrieval_anchor_literals_from_text(current_text).is_empty() {
        return None;
    }

    let current_is_short_follow_up = is_context_dependent_follow_up(current_text);
    let mut fallback_scope = None;
    for turn in previous_turns
        .iter()
        .rev()
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
    {
        if let Some(source) = focused_or_subject_conversation_turn_text(
            turn.content_text,
            focus_tokens,
            subject_tokens,
        ) {
            let anchors = extract_retrieval_anchor_literals_from_text(&source);
            if !anchors.is_empty() {
                return render_literal_anchor_scope(&anchors);
            }
            if is_compact_literal_memory(turn.content_text) {
                let anchors = extract_retrieval_anchor_literals_from_text(turn.content_text);
                if !anchors.is_empty() {
                    return render_literal_anchor_scope(&anchors);
                }
            }
            continue;
        }

        let anchors = extract_retrieval_anchor_literals_from_text(turn.content_text);
        if anchors.is_empty() {
            continue;
        }
        if current_is_short_follow_up && fallback_scope.is_none() {
            fallback_scope = render_literal_anchor_scope(&anchors);
        } else if !current_is_short_follow_up {
            break;
        }
    }

    fallback_scope
}

fn pinned_previous_assistant_literal_anchor_scope(
    previous_turns: &[&RuntimeContextTurn<'_>],
    current_text: &str,
    should_scope_with_history: bool,
) -> Option<String> {
    if !should_scope_with_history
        || !extract_retrieval_anchor_literals_from_text(current_text).is_empty()
    {
        return None;
    }

    let hidden_turn_cutoff = previous_turns
        .len()
        .saturating_sub(MAX_PROMPT_HISTORY_TURNS.max(MAX_GROUNDED_ANSWER_TOOL_HISTORY_TURNS));
    let mut anchors = Vec::new();
    for turn in previous_turns
        .iter()
        .take(hidden_turn_cutoff)
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
    {
        for literal in extract_backtick_literals_from_text(turn.content_text) {
            push_retrieval_anchor_literal(&literal, &mut anchors);
            if anchors.len() >= MAX_HISTORY_LITERAL_ANCHOR_ITEMS {
                return render_literal_anchor_scope(&anchors);
            }
        }
    }

    render_literal_anchor_scope(&anchors)
}

fn extract_retrieval_anchor_literals_from_text(value: &str) -> Vec<String> {
    let mut anchors = Vec::new();
    for literal in extract_backtick_literals_from_text(value) {
        push_retrieval_anchor_literal(&literal, &mut anchors);
        if anchors.len() >= MAX_HISTORY_LITERAL_ANCHOR_ITEMS {
            break;
        }
    }
    anchors
}

fn push_retrieval_anchor_literal(candidate: &str, anchors: &mut Vec<String>) {
    let candidate = candidate.trim();
    let char_count = candidate.chars().count();
    if candidate.is_empty()
        || char_count > MAX_HISTORY_LITERAL_CHARS
        || candidate.chars().any(char::is_whitespace)
        || anchors.iter().any(|existing| existing == candidate)
    {
        return;
    }
    let has_alphanumeric = candidate.chars().any(char::is_alphanumeric);
    let has_structural = candidate.chars().any(|ch| !ch.is_alphanumeric());
    if !has_alphanumeric || (!literal_text_is_identifier_shaped(candidate) && !has_structural) {
        return;
    }
    anchors.push(candidate.to_string());
}

fn render_literal_anchor_scope(anchors: &[String]) -> Option<String> {
    let mut line = HISTORY_LITERAL_ANCHOR_PREFIX.to_string();
    let mut rendered = 0usize;
    for anchor in anchors {
        let separator = if rendered == 0 { " " } else { ", " };
        let fragment = format!("{separator}`{anchor}`");
        if line.chars().count().saturating_add(fragment.chars().count())
            > MAX_HISTORY_LITERAL_ANCHOR_CHARS
        {
            break;
        }
        line.push_str(&fragment);
        rendered = rendered.saturating_add(1);
    }

    (rendered > 0).then_some(line)
}

fn render_effective_query_text(
    previous_turns: &[&RuntimeContextTurn<'_>],
    current_text: &str,
) -> Option<String> {
    let focus_tokens = effective_query_focus_tokens(current_text);
    let subject_tokens = conversation_subject_tokens(previous_turns);
    let topic_scoped_standalone =
        topic_scoped_standalone_query(current_text, &focus_tokens, &subject_tokens);
    let literal_anchor_scope = previous_assistant_literal_anchor_scope(
        previous_turns,
        current_text,
        &focus_tokens,
        &subject_tokens,
    );
    let focused_lines = if focus_tokens.is_empty() {
        Vec::new()
    } else {
        previous_turns
            .iter()
            .rev()
            .filter_map(|turn| focused_conversation_turn_text(&turn.content_text, &focus_tokens))
            .take(MAX_EFFECTIVE_QUERY_HISTORY_TURNS)
            .collect::<Vec<_>>()
    };
    let mut lines = if topic_scoped_standalone {
        let mut lines = Vec::new();
        if let Some(topic) = latest_previous_user_subject_scope(previous_turns, &focus_tokens) {
            lines.push(topic);
        }
        if let Some(entity_scope) =
            previous_assistant_entity_scope(previous_turns, &focus_tokens, &subject_tokens)
        {
            lines.push(entity_scope);
        }
        lines
    } else if !focused_lines.is_empty() {
        let mut lines = focused_lines;
        if let Some(entity_scope) =
            previous_assistant_entity_scope(previous_turns, &focus_tokens, &subject_tokens)
        {
            lines.insert(0, entity_scope);
        }
        if let Some(anchor) = latest_previous_user_turn_text(previous_turns) {
            lines.push(anchor);
        }
        lines
    } else if !focus_tokens.is_empty() {
        let mut lines = Vec::new();
        if let Some(entity_scope) =
            previous_assistant_entity_scope(previous_turns, &focus_tokens, &subject_tokens)
        {
            lines.push(entity_scope);
        }
        if let Some(anchor) = latest_previous_user_turn_text(previous_turns) {
            lines.push(anchor);
        }
        lines
    } else {
        let mut lines = previous_turns
            .iter()
            .rev()
            .filter_map(|turn| {
                let text = compact_conversation_turn_text(
                    &turn.content_text,
                    MAX_EFFECTIVE_QUERY_TURN_CHARS,
                );
                (!text.is_empty()).then_some(text)
            })
            .take(MAX_EFFECTIVE_QUERY_HISTORY_TURNS)
            .collect::<Vec<_>>();
        if let Some(entity_scope) = latest_previous_assistant_entity_scope(previous_turns) {
            lines.insert(0, entity_scope);
        }
        lines
    };
    if !topic_scoped_standalone && let Some(anchor_scope) = literal_anchor_scope {
        lines.insert(0, anchor_scope);
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    dedup_history_lines(&mut lines);
    let scope_text = lines.join("\n");
    Some(format!(
        "{EFFECTIVE_QUERY_SCOPE_PREFIX} {scope_text}\n{EFFECTIVE_QUERY_QUESTION_PREFIX} {current_text}"
    ))
}

fn topic_scoped_standalone_query(
    current_text: &str,
    focus_tokens: &[String],
    subject_tokens: &[String],
) -> bool {
    !is_context_dependent_follow_up(current_text)
        && !focus_tokens.is_empty()
        && !subject_tokens.is_empty()
        && history_focus_overlap_count(current_text, subject_tokens) >= 1
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
            let text =
                compact_history_turn_text(&turn.turn_kind, &turn.content_text, max_chars_per_turn);
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

fn render_user_turn_history(
    turns: &[&RuntimeContextTurn<'_>],
    limit: usize,
    max_chars_per_turn: usize,
) -> Option<String> {
    let selected = turns
        .iter()
        .rev()
        .filter_map(|turn| {
            if !matches!(turn.turn_kind, QueryTurnKind::User) {
                return None;
            }
            let text = compact_conversation_turn_text(&turn.content_text, max_chars_per_turn);
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
            let text =
                compact_history_turn_text(&turn.turn_kind, &turn.content_text, max_chars_per_turn);
            (!text.is_empty()).then(|| (*turn, text))
        })
        .take(limit)
        .collect::<Vec<_>>();
    selected.reverse();
    selected
        .into_iter()
        .map(|(turn, text)| match &turn.turn_kind {
            QueryTurnKind::User => ChatMessage::user(text),
            QueryTurnKind::Assistant => assistant_history_message(text),
            QueryTurnKind::System => ChatMessage::assistant_text(format!("System note: {text}")),
            QueryTurnKind::Tool => ChatMessage::assistant_text(format!("Tool observation: {text}")),
        })
        .collect()
}

fn assistant_history_message(text: String) -> ChatMessage {
    if is_compact_literal_memory(&text) {
        return ChatMessage::system(format!(
            "Prior assistant compact literal memory. Use it only to preserve exact anchors; never copy it as a user-facing answer.\n{text}"
        ));
    }
    ChatMessage::assistant_text(text)
}

fn pinned_literal_anchor_message(anchor_scope: String) -> ChatMessage {
    ChatMessage::system(format!(
        "Prior assistant pinned literal anchors. Use them only to resolve follow-up scope and preserve exact anchors; call tools again for factual support.\n{anchor_scope}"
    ))
}

fn is_compact_literal_memory(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with(DENSE_HISTORY_LITERAL_PREFIX))
}

fn render_grounded_answer_tool_history(
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
                    &turn.content_text,
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

fn latest_previous_user_turn_text(previous_turns: &[&RuntimeContextTurn<'_>]) -> Option<String> {
    previous_turns.iter().rev().find(|turn| matches!(turn.turn_kind, QueryTurnKind::User)).and_then(
        |turn| {
            let text =
                compact_conversation_turn_text(&turn.content_text, MAX_EFFECTIVE_QUERY_TURN_CHARS);
            (!text.is_empty()).then_some(text)
        },
    )
}

fn latest_previous_user_subject_scope(
    previous_turns: &[&RuntimeContextTurn<'_>],
    focus_tokens: &[String],
) -> Option<String> {
    previous_turns
        .iter()
        .rev()
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::User))
        .find_map(|turn| {
            let turn_focus_tokens = effective_query_focus_tokens(turn.content_text);
            if turn_focus_tokens.is_empty()
                || turn_focus_tokens.len() > CONTEXT_DEPENDENT_FOLLOW_UP_MAX_TOKENS
                || focus_tokens
                    .iter()
                    .filter(|focus| {
                        turn_focus_tokens.iter().any(|candidate| near_token_match(focus, candidate))
                    })
                    .take(1)
                    .count()
                    == 0
            {
                return None;
            }
            let text =
                compact_conversation_turn_text(&turn.content_text, MAX_EFFECTIVE_QUERY_TURN_CHARS);
            (!text.is_empty()).then(|| format!("topic: {text}"))
        })
}

fn previous_assistant_context_source(
    previous_turns: &[&RuntimeContextTurn<'_>],
    focus_tokens: &[String],
    subject_tokens: &[String],
) -> Option<String> {
    let mut fallback = None;
    for turn in previous_turns
        .iter()
        .rev()
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
    {
        if let Some(source) = focused_or_subject_conversation_turn_text(
            turn.content_text,
            focus_tokens,
            subject_tokens,
        ) {
            if !source.is_empty() {
                return Some(source);
            }
        }
        if fallback.is_none() {
            fallback = Some(compact_conversation_turn_text(
                turn.content_text,
                MAX_EFFECTIVE_QUERY_TURN_CHARS,
            ));
        }
    }
    fallback.filter(|value| !value.is_empty())
}

fn focused_or_subject_conversation_turn_text(
    value: &str,
    focus_tokens: &[String],
    subject_tokens: &[String],
) -> Option<String> {
    (!focus_tokens.is_empty())
        .then(|| focused_conversation_turn_text(value, focus_tokens))
        .flatten()
        .or_else(|| {
            (!subject_tokens.is_empty())
                .then(|| focused_conversation_turn_text(value, subject_tokens))
                .flatten()
        })
}

fn previous_assistant_entity_scope(
    previous_turns: &[&RuntimeContextTurn<'_>],
    focus_tokens: &[String],
    subject_tokens: &[String],
) -> Option<String> {
    let entities = previous_assistant_context_source(previous_turns, focus_tokens, subject_tokens)
        .map(|source| extract_entities_from_previous_answer(&source))
        .unwrap_or_default();
    if entities.is_empty() {
        return None;
    }
    Some(format!(
        "entities: {}",
        entities
            .into_iter()
            .take(MAX_EFFECTIVE_QUERY_ENTITY_SCOPE_ITEMS)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn latest_previous_assistant_entity_scope(
    previous_turns: &[&RuntimeContextTurn<'_>],
) -> Option<String> {
    let entities = previous_turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| extract_entities_from_previous_answer(&turn.content_text))
        .unwrap_or_default();
    if entities.is_empty() {
        return None;
    }
    Some(format!(
        "entities: {}",
        entities
            .into_iter()
            .take(MAX_EFFECTIVE_QUERY_ENTITY_SCOPE_ITEMS)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn conversation_subject_tokens(previous_turns: &[&RuntimeContextTurn<'_>]) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for turn in previous_turns
        .iter()
        .rev()
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::User))
        .take(HISTORY_SUBJECT_LOOKBACK_TURNS)
    {
        let turn_tokens = effective_query_focus_tokens(turn.content_text);
        if turn_tokens.is_empty() || turn_tokens.len() > CONTEXT_DEPENDENT_FOLLOW_UP_MAX_TOKENS {
            continue;
        }
        for token in turn_tokens {
            if tokens.iter().any(|existing| near_token_match(existing.as_str(), &token)) {
                continue;
            }
            tokens.push(token);
            if tokens.len() >= HISTORY_SUBJECT_TOKEN_CAP {
                return tokens;
            }
        }
    }
    tokens
}

fn dedup_history_lines(lines: &mut Vec<String>) {
    let mut seen = HashSet::new();
    lines.retain(|line| seen.insert(line.to_lowercase()));
}

fn dedup_preserve_order(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.to_lowercase()));
}

fn focused_conversation_turn_text(value: &str, focus_tokens: &[String]) -> Option<String> {
    let segments = conversation_text_segments(value)
        .into_iter()
        .filter(|segment| !is_dense_history_literal_summary(segment))
        .filter(|segment| segment_mentions_focus_token(segment, focus_tokens))
        .map(|segment| compact_conversation_turn_text(segment, MAX_EFFECTIVE_QUERY_TURN_CHARS))
        .filter(|segment| !segment.is_empty())
        .take(3)
        .collect::<Vec<_>>();
    if segments.is_empty() {
        None
    } else {
        Some(compact_conversation_turn_text(&segments.join(" "), MAX_EFFECTIVE_QUERY_TURN_CHARS))
    }
}

fn conversation_text_segments(value: &str) -> Vec<&str> {
    let mut segments =
        value.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if segments.len() > 1 {
        return segments;
    }
    segments.clear();
    let mut start = 0;
    let mut in_backtick_literal = false;
    for (index, ch) in value.char_indices() {
        if ch == '`' {
            in_backtick_literal = !in_backtick_literal;
            continue;
        }
        if in_backtick_literal {
            continue;
        }
        if matches!(ch, '.' | '!' | '?' | ';') {
            let segment = value[start..index].trim();
            if !segment.is_empty() {
                segments.push(segment);
            }
            start = index + ch.len_utf8();
        }
    }
    let tail = value[start..].trim();
    if !tail.is_empty() {
        segments.push(tail);
    }
    segments
}

fn is_dense_history_literal_summary(segment: &str) -> bool {
    let trimmed = segment.trim_start();
    trimmed.starts_with(DENSE_HISTORY_LITERAL_PREFIX)
        || trimmed.starts_with(HISTORY_LITERAL_ANCHOR_PREFIX)
}

fn segment_mentions_focus_token(segment: &str, focus_tokens: &[String]) -> bool {
    if focus_tokens.is_empty() {
        return false;
    }
    let segment_lower = segment.to_lowercase();
    if focus_tokens.iter().any(|token| segment_lower.contains(token)) {
        return true;
    }
    let segment_tokens = normalized_alnum_tokens(segment, 4);
    focus_tokens
        .iter()
        .any(|focus| segment_tokens.iter().any(|candidate| near_token_match(focus, candidate)))
}

fn effective_query_focus_tokens(value: &str) -> Vec<String> {
    normalized_alnum_tokens(strip_leading_question_marker(value), 4).into_iter().collect()
}

fn is_history_overlapping_follow_up(
    previous_turns: &[&RuntimeContextTurn<'_>],
    current_text: &str,
    focus_tokens: &[String],
) -> bool {
    if previous_turns.is_empty()
        || focus_tokens.len() < HISTORY_OVERLAP_FOLLOW_UP_MIN_MATCHES
        || !extract_retrieval_anchor_literals_from_text(current_text).is_empty()
    {
        return false;
    }

    if focus_tokens.len() > HISTORY_OVERLAP_FOLLOW_UP_MAX_TOKENS {
        return dense_literal_history_overlapping_follow_up(previous_turns, focus_tokens);
    }

    previous_turns
        .iter()
        .rev()
        .take(HISTORY_OVERLAP_FOLLOW_UP_LOOKBACK_TURNS)
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::User | QueryTurnKind::Assistant))
        .any(|turn| {
            history_focus_overlap_count(turn.content_text, focus_tokens)
                >= HISTORY_OVERLAP_FOLLOW_UP_MIN_MATCHES
        })
}

fn dense_literal_history_overlapping_follow_up(
    previous_turns: &[&RuntimeContextTurn<'_>],
    focus_tokens: &[String],
) -> bool {
    if focus_tokens.len() > DENSE_LITERAL_HISTORY_FOLLOW_UP_MAX_TOKENS {
        return false;
    }
    previous_turns
        .iter()
        .rev()
        .take(HISTORY_OVERLAP_FOLLOW_UP_LOOKBACK_TURNS)
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .any(|turn| {
            extract_retrieval_anchor_literals_from_text(turn.content_text).len()
                >= DENSE_HISTORY_LITERAL_MIN_COUNT
                && history_focus_overlap_count(turn.content_text, focus_tokens)
                    >= HISTORY_OVERLAP_FOLLOW_UP_MIN_MATCHES
        })
}

fn history_focus_overlap_count(value: &str, focus_tokens: &[String]) -> usize {
    let history_tokens = normalized_alnum_tokens(value, 4);
    focus_tokens
        .iter()
        .filter(|focus| history_tokens.iter().any(|candidate| near_token_match(focus, candidate)))
        .take(HISTORY_OVERLAP_FOLLOW_UP_MIN_MATCHES)
        .count()
}

fn conversation_turn_speaker(turn_kind: &QueryTurnKind) -> &'static str {
    match turn_kind {
        QueryTurnKind::Assistant => "Assistant",
        _ => "User",
    }
}

/// Length-based follow-up heuristic used **only** to decide whether the
/// retrieval stage should sharpen the current query with entities from
/// the previous answer. Runs before `QueryCompiler`, so a short question
/// with prior turns almost always benefits from entity expansion.
/// Length cutoff is language-agnostic.
fn is_context_dependent_follow_up(value: &str) -> bool {
    let token_count = value
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .count();
    (1..=CONTEXT_DEPENDENT_FOLLOW_UP_MAX_TOKENS).contains(&token_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(turn_kind: QueryTurnKind, content_text: &str) -> RuntimeContextTurn<'_> {
        RuntimeContextTurn { turn_kind, content_text }
    }

    #[test]
    fn dense_literal_history_scopes_long_follow_up_without_new_literals() {
        let previous = [
            turn(QueryTurnKind::User, "Alpha Connector setup inventory"),
            turn(
                QueryTurnKind::Assistant,
                concat!(
                    "literals: `alpha-pkg`, `/opt/alpha/alpha.conf`, `[Main]`, ",
                    "`alphaUrl`, `alphaTimeout`, `alphaSecret`, `alphaCurrency`, ",
                    "`alphaPayload`, `alphaVisible`\n",
                    "Configuration fragments and example values are available."
                ),
            ),
            turn(
                QueryTurnKind::User,
                "complete configuration fragment inventory example values without own literals please",
            ),
        ];

        let context = build_conversation_runtime_context_from_views(&previous, false);

        assert!(context.contextual_follow_up);
        assert!(!context.grounded_answer_tool_history.is_empty());
    }

    #[test]
    fn dense_literal_history_does_not_scope_unrelated_long_question() {
        let previous = [
            turn(QueryTurnKind::User, "Alpha Connector setup inventory"),
            turn(
                QueryTurnKind::Assistant,
                concat!(
                    "literals: `alpha-pkg`, `/opt/alpha/alpha.conf`, `[Main]`, ",
                    "`alphaUrl`, `alphaTimeout`, `alphaSecret`, `alphaCurrency`, ",
                    "`alphaPayload`, `alphaVisible`"
                ),
            ),
            turn(
                QueryTurnKind::User,
                "Zeta Widget deployment migration records independent summary overview details",
            ),
        ];

        let context = build_conversation_runtime_context_from_views(&previous, false);

        assert!(!context.contextual_follow_up);
    }

    #[test]
    fn dense_literal_history_does_not_override_current_literal_scope() {
        let previous = [
            turn(QueryTurnKind::User, "Alpha Connector setup inventory"),
            turn(
                QueryTurnKind::Assistant,
                concat!(
                    "literals: `alpha-pkg`, `/opt/alpha/alpha.conf`, `[Main]`, ",
                    "`alphaUrl`, `alphaTimeout`, `alphaSecret`, `alphaCurrency`, ",
                    "`alphaPayload`, `alphaVisible`\n",
                    "Configuration fragments and example values are available."
                ),
            ),
            turn(
                QueryTurnKind::User,
                "complete configuration fragment inventory for `/opt/beta/beta.conf` instead",
            ),
        ];

        let context = build_conversation_runtime_context_from_views(&previous, false);

        assert!(!context.contextual_follow_up);
    }
}
