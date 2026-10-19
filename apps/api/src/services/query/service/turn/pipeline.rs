use super::*;

/// Emits the single completion log line shared by both turn surfaces.
///
/// Per-stage timings live on `runtime_stage_record` in Postgres (written via
/// `record_query_runtime_stage` during each phase); this line lets operators
/// filter the completion event to get one latency number per turn without
/// joining the stage table, then drill down if needed.
fn log_query_turn_completed(
    message: &'static str,
    total_ms: u64,
    terminal_execution: &query_repository::QueryExecutionRow,
    stage_summary_count: usize,
) {
    tracing::info!(
        total_ms,
        execution_id = %terminal_execution.id,
        library_id = %terminal_execution.library_id,
        conversation_id = %terminal_execution.conversation_id,
        turn_count = terminal_execution.turn_count,
        stage_summary_count,
        "{message}"
    );
}

/// Persists readable-content coverage and resolves the finished detail for a
/// grounded answer turn, opportunistically storing the result-cache winner.
async fn complete_grounded_turn(
    service: &QueryService,
    state: &AppState,
    cache_context: Option<&QueryResultCacheContext>,
    terminal_execution_id: Uuid,
    readable_content_coverage: QueryReadableContentCoverage,
) -> Result<QueryExecutionDetail, ApiError> {
    persist_query_readable_content_coverage(
        state,
        terminal_execution_id,
        readable_content_coverage,
    )
    .await?;
    let detail = service.get_execution(state, terminal_execution_id).await?;
    if let Some(cache_context) = cache_context {
        store_query_result_cache_winner(state, cache_context, &detail).await;
    }
    Ok(detail)
}

pub(super) struct QueryExecutionInterruptionGuard {
    postgres: sqlx::PgPool,
    execution_id: Uuid,
    runtime_execution_id: Uuid,
    async_operation_id: Uuid,
    armed: bool,
}

impl QueryExecutionInterruptionGuard {
    fn new(
        postgres: &sqlx::PgPool,
        execution_id: Uuid,
        runtime_execution_id: Uuid,
        async_operation_id: Uuid,
    ) -> Self {
        Self {
            postgres: postgres.clone(),
            execution_id,
            runtime_execution_id,
            async_operation_id,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for QueryExecutionInterruptionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                execution_id = %self.execution_id,
                "query execution was interrupted without an active cleanup runtime"
            );
            return;
        };
        let postgres = self.postgres.clone();
        let execution_id = self.execution_id;
        let runtime_execution_id = self.runtime_execution_id;
        let async_operation_id = self.async_operation_id;
        runtime.spawn(async move {
            match query_repository::cancel_interrupted_execution(
                &postgres,
                execution_id,
                runtime_execution_id,
                async_operation_id,
            )
            .await
            {
                Ok(true) => tracing::warn!(
                    %execution_id,
                    %runtime_execution_id,
                    "canceled interrupted query execution"
                ),
                Ok(false) => {}
                Err(error) => tracing::error!(
                    %error,
                    %execution_id,
                    %runtime_execution_id,
                    "failed to cancel interrupted query execution"
                ),
            }
        });
    }
}

/// Opens the async operation both turn surfaces report progress through, and
/// advances it to `processing` in one step.
///
/// Creating and immediately advancing it is a single logical act: an operation
/// left at `accepted` would be visible to clients as work that never started.
///
/// # Errors
///
/// Returns [`ApiError`] when the operation cannot be created or advanced.
async fn open_query_execution_operation(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    execution_id: Uuid,
    surface_kind: RuntimeSurfaceKind,
    requested_by_principal_id: Option<Uuid>,
) -> Result<crate::domains::ops::OpsAsyncOperation, ApiError> {
    let async_operation = state
        .application_services
        .ops
        .create_async_operation(
            state,
            CreateAsyncOperationCommand {
                workspace_id: conversation.workspace_id,
                library_id: Some(conversation.library_id),
                operation_kind: "query_execution".to_string(),
                surface_kind: surface_kind.as_str().to_string(),
                requested_by_principal_id,
                status: "accepted".to_string(),
                subject_kind: "query_execution".to_string(),
                subject_id: Some(execution_id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;
    state
        .application_services
        .ops
        .update_async_operation(
            state,
            crate::services::ops::service::UpdateAsyncOperationCommand {
                operation_id: async_operation.id,
                status: "processing".to_string(),
                completed_at: None,
                failure_code: None,
            },
        )
        .await
}

/// The runtime and persistence identities a turn executes against.
///
/// The query execution id is not surfaced separately: it is `execution.id`,
/// and duplicating it would create a second source of truth for one identity.
struct SeededTurnExecution {
    execution_context_bundle_id: Uuid,
    runtime_session: RuntimeExecutionSession,
    runtime_execution_id: Uuid,
    provider_execution_context: QueryProviderExecutionContext,
    execution: query_repository::QueryExecutionRow,
}

/// Inputs for [`seed_turn_execution`].
struct SeedTurnExecutionRequest<'a> {
    state: &'a AppState,
    conversation: &'a query_repository::QueryConversationRow,
    conversation_context: &'a ConversationRuntimeContext,
    request_turn_id: Uuid,
    content_text: &'a str,
    binding_id: Option<Uuid>,
    surface_kind: RuntimeSurfaceKind,
}

/// Seeds the runtime session and persists the query execution row shared by
/// both turn surfaces.
///
/// # Errors
///
/// Returns [`ApiError`] when the runtime session cannot be seeded or the
/// execution row cannot be persisted.
async fn seed_turn_execution(
    request: SeedTurnExecutionRequest<'_>,
) -> Result<SeededTurnExecution, ApiError> {
    let SeedTurnExecutionRequest {
        state,
        conversation,
        conversation_context,
        request_turn_id,
        content_text,
        binding_id,
        surface_kind,
    } = request;
    let execution_id = Uuid::now_v7();
    let execution_context_bundle_id = Uuid::now_v7();
    let runtime_session =
        seed_query_runtime_session(state, execution_id, conversation_context, surface_kind).await?;
    let runtime_execution_id = runtime_session.execution.id;
    let provider_execution_context = QueryProviderExecutionContext {
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        query_execution_id: execution_id,
        runtime_execution_id,
    };
    let execution = query_repository::create_execution(
        &state.persistence.postgres,
        &query_repository::NewQueryExecution {
            execution_id,
            context_bundle_id: execution_context_bundle_id,
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            conversation_id: conversation.id,
            request_turn_id: Some(request_turn_id),
            response_turn_id: None,
            binding_id,
            runtime_execution_id,
            query_text: content_text,
            failure_code: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(SeededTurnExecution {
        execution_context_bundle_id,
        runtime_session,
        runtime_execution_id,
        provider_execution_context,
        execution,
    })
}

impl QueryService {
    pub async fn execute_grounded_answer_turn(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Mark this scope as the latency-sensitive query lane so its provider
        // calls draw on the query-reserved budget and never starve behind an
        // ingest burst.
        Box::pin(crate::integrations::provider_budget::with_lane(
            crate::integrations::provider_budget::ProviderLane::Query,
            crate::services::query::turn_admission::with_query_turn_admission(
                state.query_turn_admission.clone(),
                self.execute_grounded_answer_pipeline(state, command),
            ),
        ))
        .await
    }

    pub async fn execute_assistant_agent_turn(
        &self,
        state: &AppState,
        auth: &AuthContext,
        command: ExecuteConversationTurnCommand,
        activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Mark this scope as the latency-sensitive query lane so every provider
        // call in the agent loop draws on the query-reserved budget and never
        // starves behind an ingest burst.
        Box::pin(crate::integrations::provider_budget::with_lane(
            crate::integrations::provider_budget::ProviderLane::Query,
            crate::services::query::turn_admission::with_query_turn_admission(
                state.query_turn_admission.clone(),
                self.execute_assistant_agent_turn_inner(state, auth, command, activity_tx),
            ),
        ))
        .await
    }

    async fn execute_assistant_agent_turn_inner<'a>(
        &'a self,
        state: &'a AppState,
        auth: &'a AuthContext,
        command: ExecuteConversationTurnCommand,
        activity_tx: Option<Sender<AgentLoopActivityEvent>>,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        let PreparedConversationTurn {
            turn_started_at,
            conversation,
            library,
            content_text,
            request_turn,
            conversation_context,
            binding_id,
            top_k,
            readable_content_identity: _,
            readable_content_coverage,
        } = prepare_conversation_turn(state, &command).await?;

        let workspace = catalog_repository::get_workspace_by_id(
            &state.persistence.postgres,
            library.workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("workspace", library.workspace_id))?;
        let library_ref = library_catalog_ref(&workspace.slug, &library.slug);
        let prior_grounded_answer_context_messages = prior_agent_grounded_answer_context(
            state,
            &conversation,
            &library,
            &conversation_context,
        )
        .await;
        let agent_conversation_history = conversation_context.prompt_history_messages.clone();
        let agent_grounded_answer_tool_history =
            conversation_context.grounded_answer_tool_history.as_slice();

        let SeededTurnExecution {
            execution_context_bundle_id,
            mut runtime_session,
            runtime_execution_id,
            provider_execution_context,
            execution,
        } = seed_turn_execution(SeedTurnExecutionRequest {
            state,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn_id: request_turn.id,
            content_text: &content_text,
            binding_id,
            surface_kind: command.surface_kind,
        })
        .await?;
        let agent_request_id = execution.id.to_string();
        let async_operation = open_query_execution_operation(
            state,
            &conversation,
            execution.id,
            command.surface_kind,
            command.author_principal_id,
        )
        .await?;
        let mut interruption_guard = QueryExecutionInterruptionGuard::new(
            &state.persistence.postgres,
            execution.id,
            runtime_execution_id,
            async_operation.id,
        );

        let outcome = execute_agent_answer_stages(AgentAnswerStageInput {
            state,
            auth,
            library: &library,
            library_ref: &library_ref,
            content_text: &content_text,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn: &request_turn,
            execution: &execution,
            execution_context_bundle_id,
            provider_execution_context,
            prior_grounded_answer_context_messages: prior_grounded_answer_context_messages
                .as_slice(),
            agent_conversation_history: agent_conversation_history.as_slice(),
            agent_grounded_answer_tool_history,
            agent_request_id: &agent_request_id,
            activity_tx,
            top_k,
            runtime_session: &mut runtime_session,
        })
        .await;

        let terminal_execution = finalize_agent_runtime_execution(
            QueryFinalization {
                state,
                command: &command,
                conversation: &conversation,
                execution: &execution,
                async_operation_id: async_operation.id,
                interruption_guard: &mut interruption_guard,
            },
            runtime_session,
            outcome,
        )
        .await?;

        persist_query_readable_content_coverage(
            state,
            terminal_execution.id,
            readable_content_coverage,
        )
        .await?;
        let detail = self.get_execution(state, terminal_execution.id).await?;
        let request_turn = detail.request_turn.ok_or(ApiError::Internal)?;
        log_query_turn_completed(
            "query.agent_turn.completed",
            turn_started_at.elapsed().as_millis() as u64,
            &terminal_execution,
            detail.runtime_stage_summaries.len(),
        );

        interruption_guard.disarm();
        Ok(QueryTurnExecutionResult {
            conversation: map_conversation_row(conversation),
            request_turn,
            response_turn: detail.response_turn,
            execution: detail.execution,
            runtime_summary: detail.runtime_summary,
            runtime_stage_summaries: detail.runtime_stage_summaries,
            context_bundle_id: execution_context_bundle_id,
            chunk_references: detail.chunk_references,
            prepared_segment_references: detail.prepared_segment_references,
            technical_fact_references: detail.technical_fact_references,
            graph_node_references: detail.graph_node_references,
            graph_edge_references: detail.graph_edge_references,
            verification_state: detail.verification_state,
            verification_warnings: detail.verification_warnings,
            answer_disposition: detail.answer_disposition,
            query_ir: None,
            clarification: detail.clarification,
        })
    }

    async fn execute_grounded_answer_pipeline(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        // Wall-clock clock for the whole turn. Captured at entry so the
        // `query.turn.completed` structured log at the bottom of this
        // function can report `total_ms` alongside the per-stage numbers
        // already persisted on `runtime_stage_record`. Turn latency on
        // a reference library is ~40 s end-to-end; this single log line
        // lets operators see which phase dominated without cross-joining
        // the stage table manually.
        let PreparedConversationTurn {
            turn_started_at,
            conversation,
            library,
            content_text,
            request_turn,
            conversation_context,
            binding_id,
            top_k,
            readable_content_identity,
            readable_content_coverage,
        } = prepare_conversation_turn(state, &command).await?;
        let cache_context =
            build_grounded_answer_cache_context(&GroundedAnswerCacheContextRequest {
                state,
                library: &library,
                readable_content_identity: &readable_content_identity,
                conversation: &conversation,
                conversation_context: &conversation_context,
                user_question: &content_text,
                top_k,
                request_turn: &request_turn,
            })
            .await?;
        let _cache_fill_guard = match self
            .coordinate_grounded_answer_cache_fill(cache_context.as_ref().map(|cache_context| {
                CacheCoordinationRequest {
                    state,
                    cache_context,
                    conversation: &conversation,
                    request_turn: &request_turn,
                }
            }))
            .await?
        {
            CacheFillCoordination::Replayed(replayed) => return Ok(*replayed),
            CacheFillCoordination::Ready(guard) => *guard,
        };

        let SeededTurnExecution {
            execution_context_bundle_id,
            mut runtime_session,
            runtime_execution_id,
            provider_execution_context,
            execution,
        } = seed_turn_execution(SeedTurnExecutionRequest {
            state,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn_id: request_turn.id,
            content_text: &content_text,
            binding_id,
            surface_kind: command.surface_kind,
        })
        .await?;
        let async_operation = open_query_execution_operation(
            state,
            &conversation,
            execution.id,
            command.surface_kind,
            command.author_principal_id,
        )
        .await?;
        let mut interruption_guard = QueryExecutionInterruptionGuard::new(
            &state.persistence.postgres,
            execution.id,
            runtime_execution_id,
            async_operation.id,
        );

        let stage_input = GroundedAnswerStageInput {
            state,
            library: &library,
            conversation: &conversation,
            conversation_context: &conversation_context,
            request_turn: &request_turn,
            execution: &execution,
            execution_context_bundle_id,
            runtime_execution_id,
            provider_execution_context,
            content_text: &content_text,
            top_k,
            include_debug: command.include_debug,
        };
        let prepared = match run_grounded_retrieve_stage(&stage_input, &mut runtime_session).await {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(finalize_grounded_retrieve_failure(
                    QueryFinalization {
                        state,
                        command: &command,
                        conversation: &conversation,
                        execution: &execution,
                        async_operation_id: async_operation.id,
                        interruption_guard: &mut interruption_guard,
                    },
                    runtime_session,
                    failure,
                )
                .await?);
            }
        };
        let turn_query_ir = Some(prepared.query_ir.clone());
        let outcome =
            run_grounded_answer_stages(&stage_input, &mut runtime_session, prepared).await;

        let terminal_execution = finalize_grounded_runtime_execution(
            QueryFinalization {
                state,
                command: &command,
                conversation: &conversation,
                execution: &execution,
                async_operation_id: async_operation.id,
                interruption_guard: &mut interruption_guard,
            },
            runtime_session,
            outcome,
        )
        .await?;

        let detail = complete_grounded_turn(
            self,
            state,
            cache_context.as_ref(),
            terminal_execution.id,
            readable_content_coverage,
        )
        .await?;
        let request_turn = detail.request_turn.ok_or(ApiError::Internal)?;

        log_query_turn_completed(
            "query.turn.completed",
            turn_started_at.elapsed().as_millis() as u64,
            &terminal_execution,
            detail.runtime_stage_summaries.len(),
        );

        interruption_guard.disarm();
        Ok(QueryTurnExecutionResult {
            conversation: map_conversation_row(conversation),
            request_turn,
            response_turn: detail.response_turn,
            execution: detail.execution,
            runtime_summary: detail.runtime_summary,
            runtime_stage_summaries: detail.runtime_stage_summaries,
            context_bundle_id: execution_context_bundle_id,
            chunk_references: detail.chunk_references,
            prepared_segment_references: detail.prepared_segment_references,
            technical_fact_references: detail.technical_fact_references,
            graph_node_references: detail.graph_node_references,
            graph_edge_references: detail.graph_edge_references,
            verification_state: detail.verification_state,
            verification_warnings: detail.verification_warnings,
            answer_disposition: detail.answer_disposition,
            query_ir: turn_query_ir,
            clarification: detail.clarification,
        })
    }
}
