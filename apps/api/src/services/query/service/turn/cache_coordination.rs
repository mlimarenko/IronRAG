use super::*;

pub(super) enum CacheFillCoordination {
    Replayed(Box<QueryTurnExecutionResult>),
    Ready(Box<Option<result_cache::QueryResultCacheFillGuard>>),
}

impl CacheFillCoordination {
    fn replayed(result: QueryTurnExecutionResult) -> Self {
        Self::Replayed(Box::new(result))
    }

    fn ready(guard: Option<result_cache::QueryResultCacheFillGuard>) -> Self {
        Self::Ready(Box::new(guard))
    }
}

enum CacheFillAttempt {
    Replayed(Box<QueryTurnExecutionResult>),
    Acquired(Box<result_cache::QueryResultCacheFillGuard>),
    Contended,
    FailOpen,
}

impl CacheFillAttempt {
    fn replayed(result: QueryTurnExecutionResult) -> Self {
        Self::Replayed(Box::new(result))
    }
}

/// The invariant inputs every cache-coordination step threads through.
///
/// Only constructed once a cache context exists; `coordinate_grounded_answer_cache_fill`
/// takes it as `Option` so the "cache disabled" path needs no branch at each
/// call site.
pub(super) struct CacheCoordinationRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) cache_context: &'a QueryResultCacheContext,
    pub(super) conversation: &'a query_repository::QueryConversationRow,
    pub(super) request_turn: &'a query_repository::QueryTurnRow,
}

fn log_cache_wait_abandoned(
    cache_context: &QueryResultCacheContext,
    conversation: &query_repository::QueryConversationRow,
) {
    tracing::info!(
        cache_key = %cache_context.cache_key,
        library_id = %conversation.library_id,
        expected_source_truth_version = cache_context.source_truth_version,
        "query result cache wait abandoned after source generation changed"
    );
}

fn log_cache_wait_timed_out(cache_context: &QueryResultCacheContext, wait_started: Instant) {
    warn!(
        cache_key = %cache_context.cache_key,
        wait_ms = wait_started.elapsed().as_millis() as u64,
        "query result cache fill wait timed out before source execution completed"
    );
}

fn log_cache_fill_lock_fail_open(error: &anyhow::Error, cache_context: &QueryResultCacheContext) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        "query result cache fill lock unavailable; proceeding without cache coordination"
    );
}

fn log_cache_fill_lock_unavailable(error: &anyhow::Error, cache_context: &QueryResultCacheContext) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        "query result cache fill lock unavailable"
    );
}

fn log_cache_source_execution_unavailable(
    error: &ApiError,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
) {
    warn!(
        error = %error,
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        "query result cache source execution is unavailable"
    );
}

fn log_cache_replay_skipped(
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    conversation: &query_repository::QueryConversationRow,
) {
    tracing::info!(
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        library_id = %conversation.library_id,
        expected_source_truth_version = cache_context.source_truth_version,
        "query result cache replay skipped after source generation changed"
    );
}

fn log_cache_hit(
    state: &AppState,
    cache_context: &QueryResultCacheContext,
    source_execution_id: Uuid,
    conversation: &query_repository::QueryConversationRow,
    request_turn: &query_repository::QueryTurnRow,
    response_turn_id: Uuid,
) {
    tracing::info!(
        stage = "query.result_cache.hit",
        cache_key = %cache_context.cache_key,
        source_execution_id = %source_execution_id,
        conversation_id = %conversation.id,
        request_turn_id = %request_turn.id,
        response_turn_id = %response_turn_id,
        semantic_rerank_mode = state.retrieval_intelligence.semantic_rerank.mode.as_str(),
        semantic_rerank_sample_scheduled = false,
        "query result replayed from source execution"
    );
}

async fn ensure_cache_fill_wait_can_continue(
    request: &CacheCoordinationRequest<'_>,
    wait_started: Instant,
    source_checked_at: &mut Instant,
) -> Result<bool, ApiError> {
    let CacheCoordinationRequest { state, cache_context, conversation, request_turn } = request;
    if source_checked_at.elapsed()
        >= result_cache::QUERY_RESULT_CACHE_NOTIFICATION_FALLBACK_INTERVAL
    {
        *source_checked_at = Instant::now();
        if !query_result_cache_source_is_current(state, cache_context).await? {
            log_cache_wait_abandoned(cache_context, conversation);
            return Ok(false);
        }
    }
    if wait_started.elapsed() < result_cache::QUERY_RESULT_CACHE_WAIT_TIMEOUT {
        return Ok(true);
    }
    log_cache_wait_timed_out(cache_context, wait_started);
    discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
    Err(ApiError::Conflict("query answer is still being prepared".to_string()))
}

async fn wait_for_cache_fill_progress(
    cache_waiter: &mut Option<result_cache::QueryResultCacheWaiter>,
    cache_context: &QueryResultCacheContext,
    wait_started: Instant,
) {
    let remaining =
        result_cache::QUERY_RESULT_CACHE_WAIT_TIMEOUT.saturating_sub(wait_started.elapsed());
    let mut use_polling_fallback = cache_waiter.is_none();
    if let Some(waiter) = cache_waiter.as_mut() {
        let wait_for =
            remaining.min(result_cache::QUERY_RESULT_CACHE_NOTIFICATION_FALLBACK_INTERVAL);
        if let Err(error) = waiter.wait_for_notification(wait_for).await {
            warn!(
                error = %error,
                cache_key = %cache_context.cache_key,
                "query result cache notification stream failed; using bounded polling"
            );
            use_polling_fallback = true;
        }
    }
    if !use_polling_fallback {
        return;
    }
    *cache_waiter = None;
    tokio::time::sleep(remaining.min(result_cache::QUERY_RESULT_CACHE_WAIT_INTERVAL)).await;
}

impl QueryService {
    /// Parks the next selected replay for this service and library outside the
    /// replay transaction, so a visibility admission can acquire its library lock.
    #[cfg(feature = "test-support")]
    pub async fn pause_next_cache_replay(
        &self,
        library_id: Uuid,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self.replay_read_pause.lock().await =
            Some(crate::services::query::service::QueryReadPause {
                library_id,
                reached: reached_tx,
                release: release_rx,
            });
        (reached, release)
    }

    #[cfg(feature = "test-support")]
    async fn pause_selected_cache_replay(&self, library_id: Uuid) -> Result<(), ApiError> {
        let pause = {
            let mut guard = self.replay_read_pause.lock().await;
            if guard.as_ref().is_some_and(|pause| pause.library_id == library_id) {
                guard.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            let _ = pause.reached.send(());
            tokio::time::timeout(std::time::Duration::from_secs(10), pause.release)
                .await
                .map_err(|_| ApiError::Conflict("cache replay test pause timed out".to_owned()))?
                .map_err(|_| ApiError::Conflict("cache replay test pause cancelled".to_owned()))?;
        }
        Ok(())
    }

    /// Runs the same selection/detail/materialization path as cache coordination,
    /// stopping at a miss instead of dispatching the paid provider fallback.
    #[cfg(feature = "test-support")]
    pub async fn replay_cached_answer_for_test(
        &self,
        state: &AppState,
        input: &query_result_cache_repository::CreateQueryExecutionReplayInput<'_>,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        let pg = &state.persistence.postgres;
        let Some(cached) = query_result_cache_repository::get_query_result_cache(
            pg,
            input.cache_key,
            input.ttl_seconds,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        else {
            return Ok(None);
        };
        let conversation = query_repository::get_conversation_by_id(pg, input.conversation_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("query_conversation", input.conversation_id)
            })?;
        let request_turn = query_repository::get_turn_by_id(pg, input.request_turn_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("query_turn", input.request_turn_id))?;
        let cache_context =
            QueryResultCacheContext::selected_for_test(cached, input.expected_source_truth_version);
        self.try_replay_query_result_cache(&CacheCoordinationRequest {
            state,
            cache_context: &cache_context,
            conversation: &conversation,
            request_turn: &request_turn,
        })
        .await
    }

    pub(super) async fn coordinate_grounded_answer_cache_fill(
        &self,
        request: Option<CacheCoordinationRequest<'_>>,
    ) -> Result<CacheFillCoordination, ApiError> {
        let Some(request) = request else {
            return Ok(CacheFillCoordination::ready(None));
        };
        let cache_context = request.cache_context;
        if let Some(replayed) = self.try_replay_query_result_cache(&request).await? {
            return Ok(CacheFillCoordination::replayed(replayed));
        }
        let wait_started = Instant::now();
        let mut source_checked_at = Instant::now();
        let mut cache_waiter = None;
        let mut cache_waiter_attempted = false;
        loop {
            match self
                .attempt_grounded_answer_cache_fill(&request, wait_started, &mut source_checked_at)
                .await?
            {
                CacheFillAttempt::Replayed(replayed) => {
                    return Ok(CacheFillCoordination::Replayed(replayed));
                }
                CacheFillAttempt::Acquired(guard) => {
                    return Ok(CacheFillCoordination::ready(Some(*guard)));
                }
                CacheFillAttempt::FailOpen => return Ok(CacheFillCoordination::ready(None)),
                CacheFillAttempt::Contended => {}
            }
            if let Some(replayed) = self
                .ensure_cache_fill_waiter(&request, &mut cache_waiter, &mut cache_waiter_attempted)
                .await?
            {
                return Ok(CacheFillCoordination::replayed(replayed));
            }
            wait_for_cache_fill_progress(&mut cache_waiter, cache_context, wait_started).await;
            if let Some(replayed) = self.try_replay_query_result_cache(&request).await? {
                return Ok(CacheFillCoordination::replayed(replayed));
            }
        }
    }

    async fn attempt_grounded_answer_cache_fill(
        &self,
        request: &CacheCoordinationRequest<'_>,
        wait_started: Instant,
        source_checked_at: &mut Instant,
    ) -> Result<CacheFillAttempt, ApiError> {
        let CacheCoordinationRequest { state, cache_context, conversation, request_turn } = request;
        let result = result_cache::try_acquire_fill_guard(
            &state.persistence.redis,
            &cache_context.cache_key,
            Uuid::now_v7(),
        )
        .await;
        match result {
            Ok(Some(guard)) => self.resolve_acquired_cache_fill_guard(request, guard).await,
            Ok(None) => {
                if !ensure_cache_fill_wait_can_continue(request, wait_started, source_checked_at)
                    .await?
                {
                    return Ok(CacheFillAttempt::FailOpen);
                }
                Ok(CacheFillAttempt::Contended)
            }
            Err(error) if result_cache::fill_lock_error_fails_open(&error) => {
                log_cache_fill_lock_fail_open(&error, cache_context);
                Ok(CacheFillAttempt::FailOpen)
            }
            Err(error) => {
                log_cache_fill_lock_unavailable(&error, cache_context);
                discard_unexecuted_query_request_turn(state, conversation, request_turn).await;
                Err(ApiError::InternalMessage(
                    "query answer coordination is unavailable".to_string(),
                ))
            }
        }
    }

    async fn resolve_acquired_cache_fill_guard(
        &self,
        request: &CacheCoordinationRequest<'_>,
        guard: result_cache::QueryResultCacheFillGuard,
    ) -> Result<CacheFillAttempt, ApiError> {
        let CacheCoordinationRequest { state, cache_context, conversation, .. } = request;
        if !query_result_cache_source_is_current(state, cache_context).await? {
            drop(guard);
            tracing::info!(
                cache_key = %cache_context.cache_key,
                library_id = %conversation.library_id,
                expected_source_truth_version = cache_context.source_truth_version,
                "query result cache fill skipped after source generation changed"
            );
            return Ok(CacheFillAttempt::FailOpen);
        }
        if let Some(replayed) = self.try_replay_query_result_cache(request).await? {
            return Ok(CacheFillAttempt::replayed(replayed));
        }
        Ok(CacheFillAttempt::Acquired(Box::new(guard)))
    }

    async fn ensure_cache_fill_waiter(
        &self,
        request: &CacheCoordinationRequest<'_>,
        cache_waiter: &mut Option<result_cache::QueryResultCacheWaiter>,
        cache_waiter_attempted: &mut bool,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        let CacheCoordinationRequest { state, cache_context, .. } = request;
        if *cache_waiter_attempted {
            return Ok(None);
        }
        *cache_waiter_attempted = true;
        match result_cache::subscribe_fill_notifications(
            &state.persistence.redis,
            &cache_context.cache_key,
        )
        .await
        {
            Ok(waiter) => {
                *cache_waiter = Some(waiter);
                self.try_replay_query_result_cache(request).await
            }
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_context.cache_key,
                    "query result cache notifications unavailable; using bounded polling"
                );
                Ok(None)
            }
        }
    }

    async fn try_replay_query_result_cache(
        &self,
        request: &CacheCoordinationRequest<'_>,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        let CacheCoordinationRequest { state, cache_context, conversation, .. } = request;
        // PostgreSQL is the authoritative winner mapping. Redis coordinates fills
        // and wakeups, but its payload is never trusted as answer provenance:
        // every replay starts from the tenant-scoped, DB-clock-fresh row.
        let cached = query_result_cache_repository::get_query_result_cache(
            &state.persistence.postgres,
            &cache_context.cache_key,
            result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(cached) = cached else {
            return Ok(None);
        };
        if cached.workspace_id != conversation.workspace_id
            || cached.library_id != conversation.library_id
        {
            evict_query_result_cache_entry(
                state,
                cache_context,
                cached.source_execution_id,
                "persistent cache winner scope does not match request",
            )
            .await;
            return Ok(None);
        }
        if result_cache::bounded_db_remaining_ttl_seconds(cached.remaining_ttl_seconds).is_none() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                cached.source_execution_id,
                "persistent cache winner expired",
            )
            .await;
            return Ok(None);
        }
        self.replay_query_result_cache_hit(request, cached.source_execution_id).await
    }

    async fn replay_query_result_cache_hit(
        &self,
        request: &CacheCoordinationRequest<'_>,
        source_execution_id: Uuid,
    ) -> Result<Option<QueryTurnExecutionResult>, ApiError> {
        let CacheCoordinationRequest { state, cache_context, conversation, request_turn } = request;
        let detail = match self.get_execution(state, source_execution_id).await {
            Ok(detail) => detail,
            Err(error) => {
                log_cache_source_execution_unavailable(&error, cache_context, source_execution_id);
                evict_query_result_cache_entry(
                    state,
                    cache_context,
                    source_execution_id,
                    "source execution unavailable",
                )
                .await;
                return Ok(None);
            }
        };
        if detail.execution.workspace_id != conversation.workspace_id
            || detail.execution.library_id != conversation.library_id
        {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution scope does not match request",
            )
            .await;
            return Ok(None);
        }
        if !detail.answer_disposition.is_factual_ready() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution is not factual-ready",
            )
            .await;
            return Ok(None);
        }
        if !query_detail_has_grounding_references(&detail) {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no grounding references",
            )
            .await;
            return Ok(None);
        }
        let Some(query_ir) = detail.query_ir.clone() else {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no compiled query IR",
            )
            .await;
            return Ok(None);
        };
        let Some(source_response_turn) = detail.response_turn.as_ref() else {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution has no response turn",
            )
            .await;
            return Ok(None);
        };
        let answer_text = source_response_turn.content_text.trim();
        if answer_text.is_empty() {
            evict_query_result_cache_entry(
                state,
                cache_context,
                source_execution_id,
                "source execution answer is empty",
            )
            .await;
            return Ok(None);
        }

        #[cfg(feature = "test-support")]
        self.pause_selected_cache_replay(conversation.library_id).await?;

        let Some((response_turn, _replay)) =
            query_result_cache_repository::create_query_execution_replay(
                &state.persistence.postgres,
                &query_result_cache_repository::CreateQueryExecutionReplayInput {
                    workspace_id: conversation.workspace_id,
                    library_id: conversation.library_id,
                    conversation_id: conversation.id,
                    request_turn_id: request_turn.id,
                    source_execution_id,
                    expected_source_truth_version: cache_context.source_truth_version,
                    cache_key: &cache_context.cache_key,
                    ttl_seconds: result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            // The library changed after this request computed its cache key.
            // Treat that as a normal miss; the caller will execute against the
            // current source state, and this old generation remains isolated
            // under its versioned key.
            log_cache_replay_skipped(cache_context, source_execution_id, conversation);
            return Ok(None);
        };
        if !query_result_cache_repository::execution_sources_are_current(
            &state.persistence.postgres,
            source_execution_id,
            conversation.workspace_id,
            conversation.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "answer visibility lookup failed"))?
        {
            return Err(ApiError::resource_not_found("query_execution", source_execution_id));
        }
        log_cache_hit(
            state,
            cache_context,
            source_execution_id,
            conversation,
            request_turn,
            response_turn.id,
        );

        Ok(Some(QueryTurnExecutionResult {
            conversation: map_conversation_row((*conversation).clone()),
            request_turn: map_turn_row((*request_turn).clone()),
            response_turn: Some(map_turn_row(response_turn)),
            context_bundle_id: detail.execution.context_bundle_id,
            execution: detail.execution,
            runtime_summary: detail.runtime_summary,
            runtime_stage_summaries: detail.runtime_stage_summaries,
            chunk_references: detail.chunk_references,
            prepared_segment_references: detail.prepared_segment_references,
            technical_fact_references: detail.technical_fact_references,
            graph_node_references: detail.graph_node_references,
            graph_edge_references: detail.graph_edge_references,
            verification_state: detail.verification_state,
            verification_warnings: detail.verification_warnings,
            answer_disposition: detail.answer_disposition,
            query_ir: Some(query_ir),
            clarification: detail.clarification,
        }))
    }
}
