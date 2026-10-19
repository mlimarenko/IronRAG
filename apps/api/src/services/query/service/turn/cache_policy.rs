use super::*;

#[derive(Debug, Clone)]
pub(super) struct QueryResultCacheContext {
    pub(super) cache_key: String,
    pub(super) library_id: Uuid,
    pub(super) source_truth_version: i64,
    readable_content_fingerprint: String,
    graph_projection_version: i64,
    graph_topology_generation: i64,
    binding_fingerprint: String,
}

impl QueryResultCacheContext {
    /// Supplies the already-selected identity to the real replay path without
    /// invoking the provider pipeline in deterministic integration tests.
    #[cfg(feature = "test-support")]
    pub(super) fn selected_for_test(
        row: query_result_cache_repository::QueryResultCacheRow,
        source_truth_version: i64,
    ) -> Self {
        Self {
            cache_key: row.cache_key,
            library_id: row.library_id,
            source_truth_version,
            readable_content_fingerprint: row.readable_content_fingerprint,
            graph_projection_version: row.graph_projection_version,
            graph_topology_generation: row.graph_topology_generation,
            binding_fingerprint: row.binding_fingerprint,
        }
    }
}

/// Inputs for building the grounded-answer result-cache context.
pub(super) struct GroundedAnswerCacheContextRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) library: &'a CatalogLibrary,
    pub(super) readable_content_identity:
        &'a crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint,
    pub(super) conversation: &'a query_repository::QueryConversationRow,
    pub(super) conversation_context: &'a ConversationRuntimeContext,
    pub(super) user_question: &'a str,
    pub(super) top_k: usize,
    pub(super) request_turn: &'a query_repository::QueryTurnRow,
}

pub(super) async fn build_grounded_answer_cache_context(
    request: &GroundedAnswerCacheContextRequest<'_>,
) -> Result<Option<QueryResultCacheContext>, ApiError> {
    let GroundedAnswerCacheContextRequest {
        state,
        readable_content_identity,
        conversation,
        request_turn,
        ..
    } = *request;
    if !query_result_cache_is_allowed(readable_content_identity) {
        return Ok(None);
    }
    if !query_result_cache_enabled_for_semantic_rerank(
        state.retrieval_intelligence.semantic_rerank.mode,
    ) {
        return Ok(None);
    }
    match build_query_result_cache_context(request).await {
        Ok(context) => Ok(Some(context)),
        Err(error) => {
            warn!(
                error = %error,
                conversation_id = %conversation.id,
                library_id = %conversation.library_id,
                "query result cache context unavailable"
            );
            discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
            Err(ApiError::InternalMessage("query answer coordination is unavailable".to_string()))
        }
    }
}

pub(super) const fn query_result_cache_is_allowed(
    readable_content_identity: &crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint,
) -> bool {
    !readable_content_identity.is_catching_up
}

pub(super) async fn discard_unexecuted_query_request_turn(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
) {
    let result = query_repository::delete_unexecuted_request_turn(
        &state.persistence.postgres,
        conversation.id,
        request_turn.id,
    )
    .await;
    match result {
        Ok(true) => log_request_discarded(conversation.id, request_turn.id),
        Ok(false) => log_request_cleanup_skipped(conversation.id, request_turn.id),
        Err(error) => log_request_discard_failed(&error, conversation.id, request_turn.id),
    }
}

fn log_request_discarded(conversation_id: Uuid, request_turn_id: Uuid) {
    tracing::debug!(
        conversation_id = %conversation_id,
        request_turn_id = %request_turn_id,
        "discarded query request before execution started"
    );
}

fn log_request_cleanup_skipped(conversation_id: Uuid, request_turn_id: Uuid) {
    tracing::debug!(
        conversation_id = %conversation_id,
        request_turn_id = %request_turn_id,
        "query request cleanup skipped because it was already accepted or removed"
    );
}

fn log_request_discard_failed<E: std::fmt::Display>(
    error: &E,
    conversation_id: Uuid,
    request_turn_id: Uuid,
) {
    warn!(
        error = %error,
        conversation_id = %conversation_id,
        request_turn_id = %request_turn_id,
        "failed to discard query request after pre-execution coordination error"
    );
}

fn log_cache_winner_source_changed(
    cache_context: &QueryResultCacheContext,
    execution_id: Uuid,
    library_id: Uuid,
) {
    tracing::info!(
        cache_key = %cache_context.cache_key,
        execution_id = %execution_id,
        library_id = %library_id,
        expected_source_truth_version = cache_context.source_truth_version,
        "query result cache winner skipped after source generation changed"
    );
}

fn log_cache_winner_store_failed<E: std::fmt::Display>(
    error: &E,
    cache_context: &QueryResultCacheContext,
    execution_id: Uuid,
) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        execution_id = %execution_id,
        "failed to store query result cache winner"
    );
}

fn log_cache_winner_redis_refresh_failed<E: std::fmt::Display>(
    error: &E,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        "failed to refresh redis query result cache winner"
    );
}

fn log_cache_winner_expired(cache_context: &QueryResultCacheContext, source_execution_id: Uuid) {
    warn!(
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        "query result cache winner expired before redis refresh"
    );
}

fn log_cache_winner_already_existed(
    cache_context: &QueryResultCacheContext,
    winner_execution_id: Uuid,
    completed_execution_id: Uuid,
) {
    warn!(
        cache_key = %cache_context.cache_key,
        winner_execution_id = %winner_execution_id,
        completed_execution_id = %completed_execution_id,
        "query result cache winner already existed"
    );
}

fn log_cache_evict_redis_failed<E: std::fmt::Display>(
    error: &E,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    reason: &'static str,
) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        reason,
        "failed to delete redis query result cache entry"
    );
}

fn log_cache_evict_postgres_failed<E: std::fmt::Display>(
    error: &E,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    reason: &'static str,
) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        reason,
        "failed to delete postgres query result cache row"
    );
}

async fn build_query_result_cache_context(
    request: &GroundedAnswerCacheContextRequest<'_>,
) -> anyhow::Result<QueryResultCacheContext> {
    let GroundedAnswerCacheContextRequest {
        state,
        library,
        readable_content_identity,
        conversation,
        conversation_context,
        user_question,
        top_k,
        request_turn: _,
    } = *request;
    anyhow::ensure!(
        library.id == conversation.library_id && library.workspace_id == conversation.workspace_id,
        "query result cache library scope does not match conversation scope"
    );
    let crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint {
        value: readable_content_fingerprint,
        source_truth_version,
        ..
    } = readable_content_identity;
    let (graph_projection_version, graph_topology_generation) =
        crate::infra::repositories::get_runtime_graph_snapshot(
            &state.persistence.postgres,
            conversation.library_id,
        )
        .await?
        .map_or((0, 0), |snapshot| {
            (snapshot.projection_version.max(0), snapshot.topology_generation.max(0))
        });
    let binding_fingerprint =
        build_query_result_binding_fingerprint(state, conversation.library_id).await?;
    let retrieval_policy_fingerprint = result_cache::retrieval_policy_fingerprint(
        &state.retrieval_intelligence,
        &state.bulk_ingest_hardening,
    );
    let library_answer_config_fingerprint = serde_json::to_string(&(
        library.include_document_hint_in_mcp_answers,
        &library.retrieval_config,
    ))?;
    let cache_key = result_cache::cache_key(&result_cache::QueryResultCacheKeyInput {
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        source_truth_version: *source_truth_version,
        library_answer_config_fingerprint: &library_answer_config_fingerprint,
        readable_content_fingerprint,
        graph_projection_version,
        graph_topology_generation,
        binding_fingerprint: &binding_fingerprint,
        retrieval_policy_fingerprint: &retrieval_policy_fingerprint,
        answer_system_prompt:
            crate::services::query::assistant_prompt::GROUNDED_SINGLE_SHOT_SYSTEM_PROMPT,
        answer_runtime_fingerprint: result_cache::answer_runtime_fingerprint(),
        mode_label: super::runtime_mode_label(GROUNDED_ANSWER_QUERY_MODE),
        top_k,
        user_question,
        effective_question: &conversation_context.current_question_text,
        answer_history_text: conversation_context.prompt_history_text.as_deref(),
    });
    Ok(QueryResultCacheContext {
        cache_key,
        library_id: conversation.library_id,
        source_truth_version: *source_truth_version,
        readable_content_fingerprint: readable_content_fingerprint.clone(),
        graph_projection_version,
        graph_topology_generation,
        binding_fingerprint,
    })
}

pub(super) async fn query_result_cache_source_is_current(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
) -> Result<bool, ApiError> {
    let current = match catalog_repository::get_library_source_truth_version(
        &state.persistence.postgres,
        cache_context.library_id,
    )
    .await
    {
        Ok(current) => current,
        Err(sqlx::Error::RowNotFound) => return Ok(false),
        Err(error) => return Err(ApiError::internal_with_log(error, "internal")),
    };
    Ok(current == cache_context.source_truth_version)
}

async fn build_query_result_binding_fingerprint(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<String> {
    let mut parts = Vec::new();
    let embedding_profile_key = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .map(|binding| binding.embedding_execution_profile_key());
    parts.push(embedding_profile_key.map_or_else(
        || "embed_profile:none".to_string(),
        |profile_key| format!("embed_profile:{profile_key}"),
    ));
    let purposes = query_result_binding_purposes(state.retrieval_intelligence.semantic_rerank.mode);
    let purpose_names =
        purposes.iter().map(|purpose| purpose.as_str().to_string()).collect::<Vec<_>>();
    let identities = ai_repository::list_effective_binding_identities(
        &state.persistence.postgres,
        library_id,
        &purpose_names,
    )
    .await?
    .into_iter()
    .map(|identity| (identity.binding_purpose.clone(), identity))
    .collect::<HashMap<_, _>>();
    for purpose in purposes {
        let part = match identities.get(purpose.as_str()) {
            Some(binding) => format!(
                "{}:{}:{}:{}:{}",
                purpose.as_str(),
                binding.id,
                binding.account_id,
                binding.model_catalog_id,
                binding.updated_at.timestamp_micros()
            ),
            None => format!("{}:none", purpose.as_str()),
        };
        parts.push(part);
    }
    Ok(parts.join("|"))
}

/// Bindings whose identity is part of the cached answer key.
///
/// Semantic rerank resolves its own `QueryRerank` binding, so a change to that
/// binding must invalidate the cached answer too. The list is one constant
/// returned for every rollout mode, so the resulting identity does not vary
/// between `Off`, `Shadow` and `Active`.
pub(super) fn query_result_binding_purposes(
    _semantic_rerank_mode: crate::domains::query::SemanticRerankMode,
) -> &'static [AiBindingPurpose] {
    const PURPOSES: &[AiBindingPurpose] = &[
        AiBindingPurpose::QueryCompile,
        AiBindingPurpose::QueryRerank,
        AiBindingPurpose::ExtractGraph,
        AiBindingPurpose::QueryAnswer,
    ];
    PURPOSES
}

pub(super) const fn query_result_cache_enabled_for_semantic_rerank(
    _mode: crate::domains::query::SemanticRerankMode,
) -> bool {
    true
}

pub(super) async fn store_query_result_cache_winner(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    detail: &QueryExecutionDetail,
) {
    if !detail.answer_disposition.is_factual_ready() {
        return;
    }
    if !query_detail_has_grounding_references(detail) {
        return;
    }
    if detail.execution.failure_code.is_some() {
        return;
    }
    let Some(response_turn) = detail.response_turn.as_ref() else {
        return;
    };
    if response_turn.content_text.trim().is_empty() {
        return;
    }
    let row = match query_result_cache_repository::upsert_query_result_cache_winner(
        &state.persistence.postgres,
        &query_result_cache_repository::UpsertQueryResultCacheInput {
            cache_key: &cache_context.cache_key,
            workspace_id: detail.execution.workspace_id,
            library_id: detail.execution.library_id,
            source_execution_id: detail.execution.id,
            expected_source_truth_version: cache_context.source_truth_version,
            readable_content_fingerprint: &cache_context.readable_content_fingerprint,
            graph_projection_version: cache_context.graph_projection_version,
            graph_topology_generation: cache_context.graph_topology_generation,
            binding_fingerprint: &cache_context.binding_fingerprint,
            ttl_seconds: result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
        },
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            log_cache_winner_source_changed(
                cache_context,
                detail.execution.id,
                detail.execution.library_id,
            );
            return;
        }
        Err(error) => {
            log_cache_winner_store_failed(&error, cache_context, detail.execution.id);
            return;
        }
    };
    if let Some(remaining_ttl_seconds) =
        result_cache::bounded_db_remaining_ttl_seconds(row.remaining_ttl_seconds)
    {
        if let Err(error) = result_cache::put_cached_execution_id(
            &state.persistence.redis,
            &cache_context.cache_key,
            row.source_execution_id,
            remaining_ttl_seconds,
        )
        .await
        {
            log_cache_winner_redis_refresh_failed(&error, cache_context, row.source_execution_id);
        }
    } else {
        log_cache_winner_expired(cache_context, row.source_execution_id);
    }
    if row.source_execution_id != detail.execution.id {
        log_cache_winner_already_existed(
            cache_context,
            row.source_execution_id,
            detail.execution.id,
        );
    }
}

pub(super) async fn evict_query_result_cache_entry(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    reason: &'static str,
) {
    if let Err(error) = result_cache::delete_cached_execution_id_if_matches(
        &state.persistence.redis,
        &cache_context.cache_key,
        source_execution_id,
    )
    .await
    {
        log_cache_evict_redis_failed(&error, cache_context, source_execution_id, reason);
    }
    if let Err(error) = query_result_cache_repository::delete_query_result_cache(
        &state.persistence.postgres,
        &cache_context.cache_key,
        source_execution_id,
    )
    .await
    {
        log_cache_evict_postgres_failed(&error, cache_context, source_execution_id, reason);
    }
}

pub(super) fn query_detail_has_grounding_references(detail: &QueryExecutionDetail) -> bool {
    !detail.chunk_references.is_empty()
        || !detail.prepared_segment_references.is_empty()
        || !detail.technical_fact_references.is_empty()
        || !detail.graph_node_references.is_empty()
        || !detail.graph_edge_references.is_empty()
}
