use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryConversation, QueryConversationDetail, QueryExecution, QueryTurn, QueryTurnKind,
    },
    infra::repositories::query_repository,
    interfaces::http::router_support::ApiError,
};

use super::{
    ConversationRuntimeContext, CreateConversationCommand, MAX_EFFECTIVE_QUERY_HISTORY_TURNS,
    MAX_EFFECTIVE_QUERY_TURN_CHARS, MAX_LIBRARY_CONVERSATIONS, MAX_PROMPT_HISTORY_TURN_CHARS,
    MAX_PROMPT_HISTORY_TURNS, QUERY_CONVERSATION_TITLE_LIMIT, QueryService,
};

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
    if turns.is_empty() {
        return ConversationRuntimeContext {
            effective_query_text: String::new(),
            prompt_history_text: None,
            coreference_entities: Vec::new(),
        };
    }
    let current_index = turns
        .iter()
        .position(|turn| turn.id == current_turn_id)
        .unwrap_or_else(|| turns.len().saturating_sub(1));
    let relevant_turns = &turns[..=current_index.min(turns.len().saturating_sub(1))];
    let current_turn = relevant_turns.last();
    let current_text = current_turn
        .map(|turn| turn.content_text.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let previous_turns =
        relevant_turns[..relevant_turns.len().saturating_sub(1)].iter().collect::<Vec<_>>();
    let prompt_history_text = render_turn_history(
        &previous_turns,
        MAX_PROMPT_HISTORY_TURNS,
        MAX_PROMPT_HISTORY_TURN_CHARS,
    );

    let coreference_entities = previous_turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| extract_entities_from_previous_answer(&turn.content_text))
        .unwrap_or_default();

    let effective_query_text = if is_context_dependent_follow_up(&current_text) {
        render_effective_query_text(&previous_turns, &current_text).unwrap_or(current_text)
    } else {
        current_text
    };

    ConversationRuntimeContext { effective_query_text, prompt_history_text, coreference_entities }
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
        .take(10)
        .collect();
    if novel.is_empty() {
        return query.to_string();
    }
    format!("{query} (context entities: {})", novel.join(", "))
}

fn extract_entities_from_previous_answer(answer: &str) -> Vec<String> {
    const COMMON_WORDS: &[&str] = &[
        "The", "This", "That", "These", "Those", "When", "Where", "What", "Which", "How", "And",
        "For", "But", "Not", "With", "From", "Into", "Also", "Here", "There", "Each", "Every",
        "Some", "Any", "All", "Both", "More", "Most", "Other", "Such", "Than", "Then", "Only",
        "Very", "Just", "About", "After", "Before", "Between", "Through", "During", "Without",
        "However", "Because", "Since", "While", "Although", "Yes", "No",
    ];

    let mut entities = Vec::new();

    // Extract backtick-enclosed terms: `PostgreSQL`, `build_router`
    let mut search_from = 0;
    while let Some(start) = answer[search_from..].find('`') {
        let abs_start = search_from + start + 1;
        if abs_start >= answer.len() {
            break;
        }
        if let Some(end) = answer[abs_start..].find('`') {
            let term = &answer[abs_start..abs_start + end];
            if term.len() > 1 && term.len() < 50 && !term.contains('\n') {
                entities.push(term.to_string());
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    // Extract capitalized multi-word sequences that look like entity names
    for word in answer.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
        if clean.len() > 2
            && clean.chars().next().map_or(false, |c| c.is_uppercase())
            && !COMMON_WORDS.contains(&clean)
        {
            entities.push(clean.to_string());
        }
    }

    entities.sort();
    entities.dedup();
    entities.truncate(20);
    entities
}

fn render_effective_query_text(
    previous_turns: &[&query_repository::QueryTurnRow],
    current_text: &str,
) -> Option<String> {
    let mut lines = previous_turns
        .iter()
        .rev()
        .filter_map(|turn| {
            let text =
                compact_conversation_turn_text(&turn.content_text, MAX_EFFECTIVE_QUERY_TURN_CHARS);
            (!text.is_empty()).then_some(text)
        })
        .take(MAX_EFFECTIVE_QUERY_HISTORY_TURNS)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    lines.push(current_text.to_string());
    Some(lines.join("\n"))
}

fn render_turn_history(
    turns: &[&query_repository::QueryTurnRow],
    limit: usize,
    max_chars_per_turn: usize,
) -> Option<String> {
    let selected = turns
        .iter()
        .rev()
        .filter_map(|turn| {
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

fn compact_conversation_turn_text(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cutoff =
        collapsed.char_indices().nth(max_chars).map_or(collapsed.len(), |(index, _)| index);
    format!("{}…", collapsed[..cutoff].trim_end())
}

fn conversation_turn_speaker(turn_kind: &QueryTurnKind) -> &'static str {
    match turn_kind {
        QueryTurnKind::Assistant => "Assistant",
        _ => "User",
    }
}

fn is_context_dependent_follow_up(value: &str) -> bool {
    const EXPLICIT_FOLLOW_UP_MARKERS: &[&str] = &[
        "да",
        "давай",
        "ага",
        "угу",
        "ок",
        "okay",
        "ok",
        "хорошо",
        "продолжай",
        "продолжи",
        "дальше",
        "ещё",
        "еще",
        "подробнее",
        "детальнее",
        "распиши",
        "пошагово",
        "покажи",
        "поясни",
        "continue",
        "go on",
        "more",
        "show me",
        "walk me through",
    ];
    const CONTEXT_WORDS: &[&str] = &[
        "это",
        "этот",
        "эта",
        "эту",
        "этом",
        "этим",
        "эти",
        "того",
        "такое",
        "такой",
        "так",
        "там",
        "тут",
        "сюда",
        "туда",
        "дальше",
        "потом",
        "здесь",
        "here",
        "there",
        "this",
        "that",
        "it",
        "them",
        "those",
        "same",
        "again",
        "further",
    ];
    const LOW_SIGNAL_WORDS: &[&str] =
        &["а", "и", "ну", "же", "ли", "бы", "please", "just", "the", "this", "that", "it"];

    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let tokens = normalized
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }
    if EXPLICIT_FOLLOW_UP_MARKERS.iter().any(|marker| {
        marker
            .contains(' ')
            .then_some(normalized.contains(marker))
            .unwrap_or_else(|| tokens.iter().any(|token| token == marker))
    }) {
        return true;
    }
    let informative_tokens = tokens
        .iter()
        .filter(|token| token.chars().count() >= 4 && !LOW_SIGNAL_WORDS.contains(token))
        .count();
    tokens.len() <= 6
        && (informative_tokens <= 1 || tokens.iter().any(|token| CONTEXT_WORDS.contains(token)))
}
