use super::*;

pub(super) fn compiled_query_uses_answer_history(question: &str, ir: &QueryIR) -> bool {
    if ir.is_follow_up() {
        return true;
    }
    ir.retrieval_query
        .as_deref()
        .map(str::trim)
        .is_some_and(|resolved| !resolved.is_empty() && resolved != question.trim())
}

fn validate_active_conversation(
    conversation: &query_repository::QueryConversationRow,
) -> Result<(), ApiError> {
    if conversation.conversation_state == QueryConversationState::Active {
        return Ok(());
    }
    Err(ApiError::Conflict(format!("conversation {} is not active", conversation.id)))
}

fn validate_conversation_library(
    conversation: &query_repository::QueryConversationRow,
    library: &CatalogLibrary,
) -> Result<(), ApiError> {
    if library.workspace_id != conversation.workspace_id {
        return Err(ApiError::Conflict(format!(
            "conversation {} has library {} outside workspace {}",
            conversation.id, library.id, conversation.workspace_id
        )));
    }
    if library.lifecycle_state != CatalogLifecycleState::Active {
        return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
    }
    Ok(())
}

async fn refresh_conversation_title(
    state: &AppState,
    conversation: query_repository::QueryConversationRow,
    content_text: &str,
) -> Result<query_repository::QueryConversationRow, ApiError> {
    let Some(derived_title) = derive_conversation_title(content_text) else {
        return Ok(conversation);
    };
    if !should_refresh_conversation_title(conversation.title.as_deref(), &derived_title) {
        return Ok(conversation);
    }
    query_repository::initialize_conversation_title(
        &state.persistence.postgres,
        conversation.id,
        &derived_title,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

/// Everything both turn pipelines resolve before they diverge.
///
/// The grounded-answer and assistant-agent turns are two surfaces over one
/// conversation model, so the work up to this point — validating the
/// conversation and its library, persisting the user turn, refreshing the
/// title, and assembling the conversation context — is identical. Keeping it
/// in one place stops the two surfaces from drifting apart.
pub(super) struct PreparedConversationTurn {
    pub(super) turn_started_at: std::time::Instant,
    pub(super) conversation: query_repository::QueryConversationRow,
    pub(super) library: CatalogLibrary,
    pub(super) content_text: String,
    pub(super) request_turn: query_repository::QueryTurnRow,
    pub(super) conversation_context: ConversationRuntimeContext,
    pub(super) binding_id: Option<Uuid>,
    pub(super) top_k: usize,
    pub(super) readable_content_identity:
        crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint,
    pub(super) readable_content_coverage: QueryReadableContentCoverage,
}

/// Validates the conversation, records the user turn, and builds the shared
/// conversation context both turn surfaces need.
///
/// # Errors
///
/// Returns [`ApiError`] when the conversation is missing or inactive, when it
/// does not belong to the resolved library, when `contentText` is empty, or
/// when a repository call fails.
pub(super) async fn prepare_conversation_turn(
    state: &AppState,
    command: &ExecuteConversationTurnCommand,
) -> Result<PreparedConversationTurn, ApiError> {
    let turn_started_at = std::time::Instant::now();
    let mut conversation = query_repository::get_conversation_by_id(
        &state.persistence.postgres,
        command.conversation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("conversation", command.conversation_id))?;
    validate_active_conversation(&conversation)?;
    let library =
        state.application_services.catalog.get_library(state, conversation.library_id).await?;
    validate_conversation_library(&conversation, &library)?;
    let readable_content_identity =
        crate::infra::repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
            &state.persistence.postgres,
            conversation.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let readable_content_coverage =
        resolve_query_readable_content_coverage(&readable_content_identity)?;

    let content_text = normalize_required_text(&command.content_text, "contentText")?;
    let request_turn = query_repository::create_turn(
        &state.persistence.postgres,
        &query_repository::NewQueryTurn {
            conversation_id: conversation.id,
            turn_kind: "user",
            author_principal_id: command.author_principal_id,
            content_text: &content_text,
            execution_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    conversation = refresh_conversation_title(state, conversation, &content_text).await?;
    let conversation_turns =
        query_repository::list_turns_by_conversation(&state.persistence.postgres, conversation.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let conversation_context = conversation_runtime_context(
        &conversation_turns,
        request_turn.id,
        &command.external_prior_turns,
    );

    let binding_id = ai_repository::get_effective_binding_by_purpose(
        &state.persistence.postgres,
        conversation.library_id,
        AiBindingPurpose::QueryAnswer.as_str(),
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .map(|binding| binding.id);

    Ok(PreparedConversationTurn {
        turn_started_at,
        conversation,
        library,
        content_text,
        request_turn,
        conversation_context,
        binding_id,
        top_k: resolve_top_k(Some(command.top_k)),
        readable_content_identity,
        readable_content_coverage,
    })
}

fn conversation_runtime_context(
    conversation_turns: &[query_repository::QueryTurnRow],
    request_turn_id: Uuid,
    external_prior_turns: &[ExternalConversationTurn],
) -> ConversationRuntimeContext {
    if external_prior_turns.is_empty() {
        return build_conversation_runtime_context(conversation_turns, request_turn_id);
    }
    build_conversation_runtime_context_with_external_history(
        conversation_turns,
        request_turn_id,
        external_prior_turns,
    )
}

pub(super) async fn prior_agent_grounded_answer_context(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    library: &CatalogLibrary,
    conversation_context: &ConversationRuntimeContext,
) -> Vec<crate::integrations::llm::ChatMessage> {
    if !should_replay_prior_grounded_answer_context(conversation_context) {
        return Vec::new();
    }
    match load_prior_grounded_answer_context_messages(state, conversation.id, library.id).await {
        Ok(messages) => messages,
        Err(error) => {
            warn!(
                error = %error,
                conversation_id = %conversation.id,
                "failed to build prior grounded-answer context replay; continuing with text history only"
            );
            Vec::new()
        }
    }
}
