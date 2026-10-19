use super::*;

use crate::domains::query::{QueryGraphEdgeReference, QueryGraphNodeReference};
use crate::services::query::service::ExecutionPreparedReferenceContext;

impl QueryService {
    /// Parks one hydrated execution read before the final provenance check.
    /// Service clones share this fixture gate; unrelated libraries cannot consume it.
    #[cfg(feature = "test-support")]
    pub async fn pause_next_execution_read(
        &self,
        library_id: Uuid,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self.execution_read_pause.lock().await =
            Some(crate::services::query::service::QueryReadPause {
                library_id,
                reached: reached_tx,
                release: release_rx,
            });
        (reached, release)
    }

    #[cfg(feature = "test-support")]
    async fn pause_hydrated_execution_read(&self, library_id: Uuid) -> Result<(), ApiError> {
        let pause = {
            let mut guard = self.execution_read_pause.lock().await;
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
                .map_err(|_| ApiError::Conflict("execution read test pause timed out".to_owned()))?
                .map_err(|_| {
                    ApiError::Conflict("execution read test pause cancelled".to_owned())
                })?;
        }
        Ok(())
    }

    pub async fn get_execution(
        &self,
        state: &AppState,
        execution_id: Uuid,
    ) -> Result<QueryExecutionDetail, ApiError> {
        let execution =
            query_repository::get_execution_by_id(&state.persistence.postgres, execution_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
        let request_turn = match execution.request_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .map(map_turn_row),
            None => None,
        };
        let response_turn = match execution.response_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .map(map_turn_row),
            None => None,
        };
        let runtime_stage_records = runtime_repository::list_runtime_stage_records(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let runtime_policy_rows = runtime_repository::list_runtime_policy_decisions(
            &state.persistence.postgres,
            execution.runtime_execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // The persisted typed outcome is authoritative control-plane state and
        // must not be discarded when the heavier evidence/reference hydration
        // times out. Load the bundle header independently and in parallel so
        // this separation does not add another serial database round trip.
        let persisted_outcome_bundle =
            state.context_store.get_bundle_by_query_execution(execution.id);
        let prepared_reference_context = tokio::time::timeout(
            REFERENCE_CONTEXT_HYDRATION_TIMEOUT,
            load_execution_prepared_reference_context(state, execution.id),
        );
        let (persisted_outcome_bundle, prepared_reference_context) =
            tokio::join!(persisted_outcome_bundle, prepared_reference_context);
        let persisted_outcome_bundle = persisted_outcome_bundle
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let (answer_disposition, clarification) = persisted_execution_answer_outcome(
            execution.id,
            persisted_outcome_bundle.as_ref().map(|bundle| &bundle.candidate_summary),
        );
        let prepared_reference_context =
            resolve_prepared_reference_context(prepared_reference_context, execution.id);

        let query_text = execution.query_text.clone();
        let query_ir = prepared_reference_context.bundle_refs.as_ref().and_then(|bundle| {
            query_ir_from_bundle_diagnostics(&bundle.bundle.assembly_diagnostics)
        });
        let (mut graph_node_references, mut graph_edge_references) =
            build_graph_references(state, &execution, &query_text, &prepared_reference_context)
                .await?;
        let mut chunk_references = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or_else(Vec::new, map_chunk_references);
        super::chunk_reference_detail::enrich_chunk_references(state, &mut chunk_references).await;
        let mut prepared_segment_references = build_prepared_segment_references(
            prepared_reference_context.bundle_refs.as_ref(),
            &prepared_reference_context.structured_block_rows,
            &prepared_reference_context.block_rank_refs,
            &query_text,
            response_turn.as_ref().map(|turn| turn.content_text.as_str()),
            &prepared_reference_context.segment_revision_info,
        );
        prepared_segment_references.extend(build_assistant_document_references(
            execution.id,
            &prepared_reference_context.assistant_document_references,
        ));
        let mut technical_fact_references = build_technical_fact_references(
            prepared_reference_context.bundle_refs.as_ref(),
            &prepared_reference_context.technical_fact_rows,
            &prepared_reference_context.fact_rank_refs,
        );
        let verification_state = prepared_reference_context
            .bundle_refs
            .as_ref()
            .map_or(QueryVerificationState::NotRun, |bundle| {
                parse_query_verification_state(&bundle.bundle.verification_state)
            });
        let verification_warnings =
            prepared_reference_context.bundle_refs.as_ref().map_or_else(Vec::new, |bundle| {
                parse_query_verification_warnings(&bundle.bundle.verification_warnings)
            });
        // A system statement about the library is not a corpus-grounded claim,
        // so no retrieved source backs it: clear every reference vector after
        // the graph backfill, before the detail is projected to any transport.
        let is_system_statement = persisted_outcome_bundle
            .as_ref()
            .is_some_and(|bundle| persisted_answer_is_system_statement(&bundle.candidate_summary));
        clear_references_for_system_statement(
            is_system_statement,
            &mut chunk_references,
            &mut prepared_segment_references,
            &mut technical_fact_references,
            &mut graph_node_references,
            &mut graph_edge_references,
        );
        if answer_disposition.is_factual_ready()
            && !is_system_statement
            && chunk_references.is_empty()
            && prepared_segment_references.is_empty()
            && technical_fact_references.is_empty()
            && graph_node_references.is_empty()
            && graph_edge_references.is_empty()
        {
            return Err(ApiError::resource_not_found("query_execution", execution_id));
        }
        #[cfg(feature = "test-support")]
        self.pause_hydrated_execution_read(execution.library_id).await?;

        if response_turn.is_some()
            && !is_system_statement
            && !query_result_cache_repository::execution_sources_are_current(
                &state.persistence.postgres,
                execution.id,
                execution.workspace_id,
                execution.library_id,
            )
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "answer visibility lookup failed")
            })?
        {
            return Err(ApiError::resource_not_found("query_execution", execution_id));
        }
        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution.clone()),
            runtime_summary: map_execution_runtime_summary(&execution, &runtime_policy_rows),
            runtime_stage_summaries: map_execution_runtime_stage_summaries(
                &execution,
                &runtime_stage_records,
            ),
            request_turn,
            response_turn,
            chunk_references,
            prepared_segment_references,
            technical_fact_references,
            graph_node_references,
            graph_edge_references,
            verification_state,
            verification_warnings,
            answer_disposition,
            clarification,
            query_ir,
        })
    }
}

fn resolve_prepared_reference_context(
    result: Result<
        Result<ExecutionPreparedReferenceContext, ApiError>,
        tokio::time::error::Elapsed,
    >,
    execution_id: Uuid,
) -> ExecutionPreparedReferenceContext {
    match result {
        Ok(Ok(reference_context)) => reference_context,
        Ok(Err(error)) => {
            log_prepared_references_failed(execution_id, &error);
            Default::default()
        }
        Err(_) => {
            log_prepared_references_timed_out(execution_id);
            Default::default()
        }
    }
}

fn log_prepared_references_failed(execution_id: Uuid, error: &ApiError) {
    warn!(
        execution_id = %execution_id,
        error = %error,
        "failed to resolve prepared references for query execution detail"
    );
}

fn log_prepared_references_timed_out(execution_id: Uuid) {
    warn!(
        execution_id = %execution_id,
        timeout_ms = REFERENCE_CONTEXT_HYDRATION_TIMEOUT.as_millis(),
        "timed out resolving prepared references for query execution detail"
    );
}

/// Backfills and hydrates the graph node/edge references for one execution.
async fn build_graph_references(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    query_text: &str,
    prepared_reference_context: &ExecutionPreparedReferenceContext,
) -> Result<(Vec<QueryGraphNodeReference>, Vec<QueryGraphEdgeReference>), ApiError> {
    let has_prepared_bundle_refs = prepared_reference_context.bundle_refs.is_some();
    let mut graph_node_references = prepared_reference_context
        .bundle_refs
        .as_ref()
        .map_or_else(Vec::new, map_entity_references);
    if should_backfill_graph_entity_references(
        has_prepared_bundle_refs,
        graph_node_references.is_empty(),
    ) {
        graph_node_references = search_runtime_graph_entity_references(
            &state.persistence.postgres,
            execution.library_id,
            execution.id,
            query_text,
        )
        .await;
    }
    let mut graph_edge_references = prepared_reference_context
        .bundle_refs
        .as_ref()
        .map_or_else(Vec::new, map_relation_references);
    if !graph_node_references.is_empty() || !graph_edge_references.is_empty() {
        let graph_projection_version = crate::infra::repositories::get_runtime_graph_snapshot(
            &state.persistence.postgres,
            execution.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .map_or(0, |snapshot| snapshot.projection_version.max(0));
        graph_node_references = hydrate_entity_references(
            &state.persistence.postgres,
            execution.library_id,
            graph_projection_version,
            graph_node_references,
        )
        .await;
        graph_edge_references = hydrate_relation_references(
            &state.persistence.postgres,
            execution.library_id,
            graph_projection_version,
            graph_edge_references,
        )
        .await;
    }
    Ok((graph_node_references, graph_edge_references))
}

pub(super) fn persisted_execution_answer_outcome(
    execution_id: Uuid,
    candidate_summary: Option<&serde_json::Value>,
) -> (crate::domains::query::QueryAnswerDisposition, QueryClarification) {
    let Some(candidate_summary) = candidate_summary else {
        return Default::default();
    };
    persisted_query_answer_outcome(candidate_summary).unwrap_or_else(|_| {
        warn!(
            %execution_id,
            "persisted query answer outcome is invalid; using conservative non-terminal outcome"
        );
        Default::default()
    })
}

pub(super) fn should_backfill_graph_entity_references(
    has_prepared_bundle_refs: bool,
    graph_node_references_empty: bool,
) -> bool {
    graph_node_references_empty && !has_prepared_bundle_refs
}

/// Clear every retrieval reference vector when the persisted answer is the
/// product's own system statement. Pure so it is testable without a database.
pub(super) fn clear_references_for_system_statement<A, B, C, D, E>(
    is_system_statement: bool,
    chunk_references: &mut Vec<A>,
    prepared_segment_references: &mut Vec<B>,
    technical_fact_references: &mut Vec<C>,
    graph_node_references: &mut Vec<D>,
    graph_edge_references: &mut Vec<E>,
) {
    if !is_system_statement {
        return;
    }
    chunk_references.clear();
    prepared_segment_references.clear();
    technical_fact_references.clear();
    graph_node_references.clear();
    graph_edge_references.clear();
}
