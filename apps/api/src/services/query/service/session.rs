use std::collections::HashSet;

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryConversation, QueryConversationDetail, QueryExecution, QueryTurn, QueryTurnKind,
    },
    infra::repositories::query_repository,
    interfaces::http::router_support::ApiError,
    services::query::text_match::{near_token_match, normalized_alnum_tokens},
};

use super::{
    ConversationRuntimeContext, CreateConversationCommand, ExternalConversationTurn,
    MAX_EFFECTIVE_QUERY_HISTORY_TURNS, MAX_EFFECTIVE_QUERY_TURN_CHARS, MAX_LIBRARY_CONVERSATIONS,
    MAX_PROMPT_HISTORY_TURN_CHARS, MAX_PROMPT_HISTORY_TURNS, QUERY_CONVERSATION_TITLE_LIMIT,
    QueryService,
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
        .enumerate()
        .map(|(index, turn)| RuntimeContextTurn {
            turn_index: i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX),
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
            turn_index: i32::try_from(views.len().saturating_add(1)).unwrap_or(i32::MAX),
            turn_kind: turn.turn_kind.clone(),
            content_text: turn.content_text.as_str(),
        });
    }
    for turn in external_prior_turns {
        views.push(RuntimeContextTurn {
            turn_index: i32::try_from(views.len().saturating_add(1)).unwrap_or(i32::MAX),
            turn_kind: turn.turn_kind.clone(),
            content_text: turn.content_text.as_str(),
        });
    }
    if let Some(current_turn) = relevant_turns.last() {
        views.push(RuntimeContextTurn {
            turn_index: i32::try_from(views.len().saturating_add(1)).unwrap_or(i32::MAX),
            turn_kind: current_turn.turn_kind.clone(),
            content_text: current_turn.content_text.as_str(),
        });
    }
    build_conversation_runtime_context_from_views(&views)
}

#[derive(Debug, Clone)]
struct RuntimeContextTurn<'a> {
    turn_index: i32,
    turn_kind: QueryTurnKind,
    content_text: &'a str,
}

fn build_conversation_runtime_context_from_views(
    turns: &[RuntimeContextTurn<'_>],
) -> ConversationRuntimeContext {
    if turns.is_empty() {
        return ConversationRuntimeContext {
            effective_query_text: String::new(),
            prompt_history_text: None,
            coreference_entities: Vec::new(),
        };
    }
    let current_turn = turns.last();
    let current_text = current_turn
        .map(|turn| turn.content_text.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let previous_turns = turns[..turns.len().saturating_sub(1)].iter().collect::<Vec<_>>();
    let is_follow_up = is_context_dependent_follow_up(&current_text);
    let follow_up_focus_tokens =
        if is_follow_up { effective_query_focus_tokens(&current_text) } else { Vec::new() };
    let prompt_history_text = if is_follow_up {
        render_follow_up_prompt_history(&previous_turns, &follow_up_focus_tokens)
    } else {
        None
    };

    let coreference_entities = previous_turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .map(|turn| {
            let focused_source = (!follow_up_focus_tokens.is_empty())
                .then(|| {
                    focused_conversation_turn_text(&turn.content_text, &follow_up_focus_tokens)
                })
                .flatten();
            extract_entities_from_previous_answer(
                focused_source.as_deref().unwrap_or(&turn.content_text),
            )
        })
        .unwrap_or_default();

    let effective_query_text = if is_follow_up {
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
    let mut entities = Vec::new();

    // Backticked terms: `PostgreSQL`, `build_router`, `/api/v1/users`.
    // These are unambiguous — the model quoted them intentionally, so
    // they are always entity-like regardless of casing.
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

    // Bare capitalised tokens fed to the coreference resolver; false
    // positives (spurious "Both" etc.) only cause a missed follow-up-
    // sharpening, never a wrong answer.
    for word in answer.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
        if clean.chars().count() >= 4 && clean.chars().next().is_some_and(char::is_uppercase) {
            entities.push(clean.to_string());
        }
    }

    entities.sort();
    entities.dedup();
    entities.truncate(20);
    entities
}

fn render_effective_query_text(
    previous_turns: &[&RuntimeContextTurn<'_>],
    current_text: &str,
) -> Option<String> {
    let focus_tokens = effective_query_focus_tokens(current_text);
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
    let mut lines = if !focused_lines.is_empty() {
        let mut lines = focused_lines;
        if let Some(anchor) = latest_previous_user_turn_text(previous_turns) {
            lines.push(anchor);
        }
        lines
    } else if !focus_tokens.is_empty() {
        latest_previous_user_turn_text(previous_turns)
            .map(|anchor| vec![anchor])
            .unwrap_or_default()
    } else {
        previous_turns
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
            .collect::<Vec<_>>()
    };
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    dedup_history_lines(&mut lines);
    lines.push(current_text.to_string());
    Some(lines.join("\n"))
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

fn render_follow_up_prompt_history(
    previous_turns: &[&RuntimeContextTurn<'_>],
    focus_tokens: &[String],
) -> Option<String> {
    let mut selected = Vec::<(i32, String)>::new();

    if let Some(user_turn) =
        previous_turns.iter().rev().find(|turn| matches!(turn.turn_kind, QueryTurnKind::User))
    {
        let text =
            compact_conversation_turn_text(&user_turn.content_text, MAX_PROMPT_HISTORY_TURN_CHARS);
        if !text.is_empty() {
            selected.push((
                user_turn.turn_index,
                format!("{}: {}", conversation_turn_speaker(&user_turn.turn_kind), text),
            ));
        }
    }

    if let Some(assistant_turn) =
        previous_turns.iter().rev().find(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
    {
        let text = focused_conversation_turn_text(&assistant_turn.content_text, focus_tokens)
            .unwrap_or_else(|| {
                compact_conversation_turn_text(
                    &assistant_turn.content_text,
                    MAX_PROMPT_HISTORY_TURN_CHARS,
                )
            });
        if !text.is_empty() {
            selected.push((
                assistant_turn.turn_index,
                format!("{}: {}", conversation_turn_speaker(&assistant_turn.turn_kind), text),
            ));
        }
    }

    if selected.is_empty() {
        return render_turn_history(
            previous_turns,
            MAX_PROMPT_HISTORY_TURNS.min(2),
            MAX_PROMPT_HISTORY_TURN_CHARS,
        );
    }

    selected.sort_by_key(|(turn_index, _)| *turn_index);
    Some(selected.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n"))
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

fn dedup_history_lines(lines: &mut Vec<String>) {
    let mut seen = HashSet::new();
    lines.retain(|line| seen.insert(line.to_lowercase()));
}

fn focused_conversation_turn_text(value: &str, focus_tokens: &[String]) -> Option<String> {
    let segments = conversation_text_segments(value)
        .into_iter()
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
    for (index, ch) in value.char_indices() {
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
    normalized_alnum_tokens(value, 4).into_iter().collect()
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
    let tokens = value
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let informative_token_count = tokens.iter().filter(|token| token.chars().count() >= 4).count();
    tokens.len() <= 2 && informative_token_count <= 1
}
