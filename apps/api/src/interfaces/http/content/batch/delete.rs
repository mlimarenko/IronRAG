use super::*;

/// Background executor for batch delete.
///
/// Each document still goes through the accepted delete-mutation path, so
/// audit/mutation/async-operation state stays identical to single-document
/// delete. The batch-specific difference is graph convergence: child deletes
/// remove only document-local artifacts, then the parent deletes graph evidence
/// for successfully deleted documents and refreshes only those graph targets.
pub(super) async fn execute_batch_delete(
    state: AppState,
    operation: BatchDocumentOperation,
) -> bool {
    let BatchDocumentOperation { parent_id, library_id, workspace_id, .. } = operation;
    let total = operation.document_ids.len();
    info!(
        %parent_id,
        %library_id,
        %workspace_id,
        total,
        parallelism = operation.parallelism,
        "batch delete started"
    );

    let state = Arc::new(state);
    let child_outcome = run_batch_delete_children(state.clone(), operation).await;
    let convergence_failure =
        converge_deleted_graph(&state, parent_id, library_id, &child_outcome, total).await;
    settle_batch_delete_parent(&state, parent_id, &child_outcome, total, convergence_failure).await
}

/// Runs the graph-convergence step over the already-deleted documents.
///
/// Graph convergence never decides the operation's outcome — the children do.
/// A failure is recorded as a detail on the parent, so a poller sees the
/// deletion's real result while the operator still sees the stale graph.
///
/// A projection refresh is skipped when every selected document failed to
/// delete: with `failure_count == total` there are no freshly deleted documents
/// to project, and the evidence cleanup still runs.
async fn converge_deleted_graph(
    state: &AppState,
    parent_id: Uuid,
    library_id: Uuid,
    child_outcome: &BatchDeleteChildOutcome,
    total: usize,
) -> Option<GraphConvergenceFailure> {
    let cleanup = match state
        .application_services
        .content
        .cleanup_deleted_documents_graph_evidence(
            state,
            library_id,
            &child_outcome.deleted_document_ids,
        )
        .await
    {
        Ok(cleanup) => cleanup,
        Err(error) => {
            log_batch_delete_graph_cleanup_failed(parent_id, library_id, &error);
            return Some(GraphConvergenceFailure::EvidenceCleanup);
        }
    };
    if !cleanup.requires_graph_convergence() {
        log_batch_delete_graph_projection_skipped(parent_id, library_id);
        return None;
    }
    if child_outcome.failure_count >= total {
        return None;
    }
    match state
        .application_services
        .content
        .refresh_deleted_library_graph_projection_for_cleanup(state, library_id, cleanup)
        .await
    {
        Ok(()) => None,
        Err(error) => {
            log_batch_delete_graph_projection_failed(parent_id, library_id, &error);
            Some(GraphConvergenceFailure::Projection)
        }
    }
}

fn log_batch_delete_graph_projection_skipped(parent_id: Uuid, library_id: Uuid) {
    info!(
        %parent_id,
        %library_id,
        "batch delete skipped graph projection because selected documents had no graph evidence"
    );
}

fn log_batch_delete_graph_cleanup_failed(
    parent_id: Uuid,
    library_id: Uuid,
    error: &impl std::fmt::Display,
) {
    error!(
        %parent_id,
        %library_id,
        error = %error,
        "batch delete final graph cleanup failed"
    );
}

fn log_batch_delete_graph_projection_failed(
    parent_id: Uuid,
    library_id: Uuid,
    error: &impl std::fmt::Display,
) {
    error!(
        %parent_id,
        %library_id,
        error = %error,
        "batch delete final graph projection failed"
    );
}

/// A background graph-convergence step that failed after the selected documents
/// were already deleted.
///
/// Both steps remove graph evidence for deletions that already happened, so a
/// failure is not a deletion failure. It is recorded as a detail on the parent
/// (merged into `result_json` by the shared settle), and an operator can re-run
/// the convergence pass. The parent's status still comes from its children.
#[derive(Debug, Clone, Copy)]
enum GraphConvergenceFailure {
    EvidenceCleanup,
    Projection,
}

impl GraphConvergenceFailure {
    const fn failure_code(self) -> &'static str {
        match self {
            Self::EvidenceCleanup => "graph_cleanup_failed",
            Self::Projection => "graph_projection_failed",
        }
    }

    const fn stage(self) -> &'static str {
        match self {
            Self::EvidenceCleanup => "evidence_cleanup",
            Self::Projection => "projection",
        }
    }

    fn detail(self) -> serde_json::Value {
        serde_json::json!({
            "graph_convergence": {
                "stage": self.stage(),
                "failure_code": self.failure_code(),
            }
        })
    }
}

/// Settles the batch-delete parent through the shared fan-out aggregate.
///
/// The aggregate derives the terminal state from the persisted children — the
/// same function the operator repair and the abnormal-exit guard use — so the
/// live path and recovery cannot drift. The in-memory failure tally is passed
/// as the fallback code the aggregate uses only when the persisted children
/// cannot decide the state: the fan-out produced no child at all (every
/// selected document was already gone), or a child is still in flight. When at
/// least one child exists and all are terminal, the children decide.
async fn settle_batch_delete_parent(
    state: &AppState,
    parent_id: Uuid,
    child_outcome: &BatchDeleteChildOutcome,
    total: usize,
    convergence_failure: Option<GraphConvergenceFailure>,
) -> bool {
    let fallback_failure_code =
        batch_delete_fallback_failure_code(child_outcome.failure_count, total);
    let detail = convergence_failure.map(GraphConvergenceFailure::detail);
    match state
        .application_services
        .ops
        .settle_fan_out_parent_from_children(
            state,
            parent_id,
            crate::services::ops::service::FanOutParentSettle {
                fallback_failure_code: fallback_failure_code.as_deref(),
                childless_status: None,
                detail: detail.as_ref(),
                guard: ops_repository::FanOutSettleGuard::InFlight,
            },
        )
        .await
    {
        Ok(Some(settled)) => {
            log_batch_delete_completed(
                parent_id,
                child_outcome.failure_count,
                total,
                settled.status.as_str(),
            );
            true
        }
        Ok(None) => {
            // The parent is already terminal, or the persisted children cannot
            // decide its state. Nothing was written; the operator repair settles
            // the second case once the children are terminal.
            log_batch_delete_nothing_to_settle(parent_id);
            true
        }
        Err(error) => {
            log_batch_delete_settle_failed(parent_id, &error);
            false
        }
    }
}

/// Fallback failure code the aggregate uses only when the persisted children
/// cannot decide the parent's state.
fn batch_delete_fallback_failure_code(failure_count: usize, total: usize) -> Option<String> {
    (failure_count > 0).then(|| format!("delete_failed:{failure_count}/{total}"))
}

fn log_batch_delete_completed(parent_id: Uuid, failure_count: usize, total: usize, status: &str) {
    info!(
        %parent_id,
        failures = failure_count,
        total,
        status = %status,
        "batch delete completed"
    );
}

fn log_batch_delete_nothing_to_settle(parent_id: Uuid) {
    warn!(%parent_id, "batch delete parent had nothing to settle");
}

fn log_batch_delete_settle_failed(parent_id: Uuid, error: &impl std::fmt::Display) {
    error!(%parent_id, error = %error, "failed to settle batch delete parent");
}

#[derive(Debug, Default)]
struct BatchDeleteChildOutcome {
    deleted_document_ids: Vec<Uuid>,
    failure_count: usize,
}

async fn run_batch_delete_children(
    state: Arc<AppState>,
    operation: BatchDocumentOperation,
) -> BatchDeleteChildOutcome {
    use futures::stream;

    let parent_id = operation.parent_id;
    let principal_id = operation.principal_id;
    let parallelism = operation.parallelism.max(1);
    let child_results = stream::iter(operation.document_ids)
        .map(|document_id| {
            let state = state.clone();
            async move {
                let result =
                    match delete_single_document(&state, Some(parent_id), principal_id, document_id)
                        .await
                    {
                        Ok(_) => Ok(document_id),
                        Err(error) => {
                            error!(%parent_id, %document_id, error = %error, "batch child delete failed");
                            Err(())
                        }
                    };
                // One-shot probe: the first completion consumes it, so this
                // pauses after the first child only. Compiled out of production
                // builds; lets a test abort the worker inside its detached
                // window and prove the abnormal-exit guard settles the parent.
                #[cfg(feature = "test-support")]
                crate::shared::cancellation::one_shot_pause_probe::pause().await;
                result
            }
        })
        .buffer_unordered(parallelism)
        .collect::<Vec<Result<Uuid, ()>>>()
        .await;
    let mut outcome = BatchDeleteChildOutcome::default();
    for result in child_results {
        match result {
            Ok(document_id) => outcome.deleted_document_ids.push(document_id),
            Err(()) => outcome.failure_count += 1,
        }
    }
    outcome
}

async fn delete_single_document(
    state: &AppState,
    parent_id: Option<Uuid>,
    principal_id: Uuid,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    let document =
        content_document_repository::get_document_by_id(&state.persistence.postgres, document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    if document.document_state == "deleted" || document.deleted_at.is_some() {
        return Err(ApiError::resource_not_found("document", document_id));
    }

    state
        .application_services
        .content
        .admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: None,
                parent_async_operation_id: parent_id,
            },
        )
        .await
}
