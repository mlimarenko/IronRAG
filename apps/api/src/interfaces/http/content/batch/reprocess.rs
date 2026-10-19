use super::*;

/// Background executor for the batch rerun. Drives children through
/// `reprocess_single_document`, tracks counts, and settles the parent
/// `ops_async_operation` at the end with either `ready` (all children
/// succeeded) or `failed` (at least one child failed). A whole-batch
/// timeout is enforced via `tokio::time::timeout`; on timeout the parent
/// is marked failed with `batch_timeout`.
pub(super) async fn execute_batch_reprocess(
    state: AppState,
    operation: BatchDocumentOperation,
    timeout: Duration,
) -> bool {
    let total = operation.document_ids.len();
    log_batch_reprocess_started(&operation, total);
    let parent_id = operation.parent_id;
    let state = Arc::new(state);

    let worker = async {
        let failure_count = run_batch_reprocess_children(state.clone(), operation).await;

        // Batch semantics: the parent row tracks the ADMIT phase. It
        // stays in `processing` until every child async_op reaches a
        // terminal state — the GET /ops/operations/{id} endpoint derives
        // the effective final status from the child progress aggregate.
        // We only touch the parent here when the admit phase itself
        // surfaces non-recoverable fan-out errors, i.e. every single doc
        // failed to even enter the pipeline. Mixed / partial admit
        // failures still leave the parent as `processing`; the children
        // that DID admit will settle on their own and the aggregate will
        // reflect both buckets correctly.
        let admit_settled = if failure_count == total && total > 0 {
            let fallback = format!("admit_failed:{failure_count}/{total}");
            match state
                .application_services
                .ops
                .settle_fan_out_parent_from_children(
                    state.as_ref(),
                    parent_id,
                    crate::services::ops::service::FanOutParentSettle {
                        fallback_failure_code: Some(&fallback),
                        childless_status: None,
                        detail: None,
                        guard: ops_repository::FanOutSettleGuard::InFlight,
                    },
                )
                .await
            {
                Ok(_) => true,
                Err(error) => {
                    log_batch_reprocess_admit_settle_failed(parent_id, &error);
                    false
                }
            }
        } else {
            false
        };
        log_batch_reprocess_admit_completed(parent_id, failure_count, total);

        // Once all docs were admitted, the child async_op rows own the
        // remaining work. The parent row stayed `processing` forever on
        // happy paths before this — the HTTP endpoint derived the
        // effective final status from child progress, but any direct
        // SQL reader (metrics, admin tools, `completed_at IS NULL`
        // queries) saw a row stuck for the library's full life. Poll
        // the aggregate count and settle the parent once every child
        // reaches a terminal state. `in_flight == 0` means every
        // `ops_async_operation` row with `parent_async_operation_id =
        // parent_id` has a `completed_at`.
        admit_settled || settle_parent_when_children_terminal(state.clone(), parent_id).await
    };

    match tokio::time::timeout(timeout, worker).await {
        Ok(settled) => settled,
        Err(_) => {
            log_batch_reprocess_timeout(parent_id, timeout);
            settle_batch_reprocess_timeout(&state, parent_id).await
        }
    }
}

fn log_batch_reprocess_started(operation: &BatchDocumentOperation, total: usize) {
    info!(
        parent_id = %operation.parent_id,
        library_id = %operation.library_id,
        workspace_id = %operation.workspace_id,
        total,
        parallelism = operation.parallelism,
        "batch reprocess started"
    );
}

fn log_batch_reprocess_timeout(parent_id: Uuid, timeout: Duration) {
    warn!(
        %parent_id,
        timeout_secs = timeout.as_secs(),
        "batch reprocess exceeded wall-clock budget; marking parent failed"
    );
}

fn log_batch_reprocess_admit_settle_failed(parent_id: Uuid, error: &impl std::fmt::Display) {
    error!(%parent_id, error = %error, "failed to settle batch parent async operation");
}

fn log_batch_reprocess_admit_completed(parent_id: Uuid, failure_count: usize, total: usize) {
    info!(%parent_id, failures = failure_count, total, "batch reprocess admit phase completed");
}

fn log_batch_reprocess_timeout_settle_failed(parent_id: Uuid, error: &impl std::fmt::Display) {
    error!(%parent_id, error = %error, "failed to mark batch parent as timed out");
}

/// Force-settles a timed-out batch-reprocess parent as failed with
/// `batch_timeout`.
async fn settle_batch_reprocess_timeout(state: &AppState, parent_id: Uuid) -> bool {
    match state
        .application_services
        .ops
        .settle_fan_out_parent_from_children(
            state,
            parent_id,
            crate::services::ops::service::FanOutParentSettle {
                fallback_failure_code: Some("batch_timeout"),
                childless_status: None,
                detail: None,
                guard: ops_repository::FanOutSettleGuard::InFlight,
            },
        )
        .await
    {
        Ok(_) => true,
        Err(error) => {
            log_batch_reprocess_timeout_settle_failed(parent_id, &error);
            false
        }
    }
}

/// Poll the batch parent's child-progress aggregate and flip the
/// parent to a terminal state once every child row settles. The
/// aggregate already exposed the effective status on the HTTP read
/// path — this routine closes the gap for any consumer that reads
/// `ops_async_operation` directly (SQL dashboards, admin tools,
/// `completed_at IS NULL` metrics).
///
/// Bounded by a fresh wall-clock budget independent of the admit
/// phase: the children run asynchronously through the ingest queue
/// long after admit returns, and the admit timeout is usually seconds
/// while child processing can take minutes. The outer
/// `tokio::time::timeout` on the worker future still fires if the
/// admit phase itself hangs; this inner loop fires only when admit
/// completed normally.
///
/// On poll-budget exhaustion the parent is LEFT as-is — the HTTP
/// read path keeps deriving the effective status from child progress
/// so UX is unchanged, and a future poll (e.g. a restart of the
/// backend process with a stale parent) can resume. No partial
/// settle: the parent either transitions to a terminal state based
/// on real child counts or stays `processing`.
async fn settle_parent_when_children_terminal(state: Arc<AppState>, parent_id: Uuid) -> bool {
    // 30 min cap: matches the worst observed reference-scale batch rerun
    // wall time (9929 docs × graph rebuild). Past this window the
    // aggregate polling itself is the bigger cost; fall through.
    const SETTLE_BUDGET: Duration = Duration::from_mins(30);
    const POLL_INTERVAL: Duration = Duration::from_secs(5);

    let deadline = tokio::time::Instant::now() + SETTLE_BUDGET;
    loop {
        let Some(progress) = load_batch_parent_progress(&state, parent_id).await else {
            return false;
        };
        if progress.total == 0 {
            return false;
        }
        if progress.in_flight == 0 {
            return settle_terminal_batch_parent(&state, parent_id).await;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                %parent_id,
                total = progress.total,
                in_flight = progress.in_flight,
                "batch parent settle budget exhausted; leaving row unsettled (effective status still derived from children)"
            );
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn load_batch_parent_progress(
    state: &AppState,
    parent_id: Uuid,
) -> Option<crate::domains::ops::OpsAsyncOperationProgress> {
    match state.application_services.ops.get_async_operation_progress(state, parent_id).await {
        Ok(progress) => Some(progress),
        Err(error) => {
            error!(
                %parent_id,
                error = %error,
                "failed to read batch parent progress — leaving parent row unsettled"
            );
            None
        }
    }
}

/// Settles the parent through the shared fan-out aggregate — the same function
/// the batch-delete worker, the abnormal-exit guard and the operator repair
/// use. Called only once every child is terminal, so the aggregate always
/// derives the state. Returns `true` when the parent's terminal state is
/// written.
async fn settle_terminal_batch_parent(state: &AppState, parent_id: Uuid) -> bool {
    match state
        .application_services
        .ops
        .settle_fan_out_parent_from_children(
            state,
            parent_id,
            crate::services::ops::service::FanOutParentSettle {
                fallback_failure_code: None,
                childless_status: None,
                detail: None,
                guard: ops_repository::FanOutSettleGuard::InFlight,
            },
        )
        .await
    {
        Ok(Some(settled)) => {
            log_batch_parent_settled(parent_id, settled.status.as_str());
            true
        }
        Ok(None) => false,
        Err(error) => {
            log_batch_parent_settle_failed(parent_id, &error);
            false
        }
    }
}

fn log_batch_parent_settled(parent_id: Uuid, status: &str) {
    info!(
        %parent_id,
        status = %status,
        "batch parent settled on children terminal"
    );
}

fn log_batch_parent_settle_failed(parent_id: Uuid, error: &impl std::fmt::Display) {
    error!(%parent_id, error = %error, "failed to settle batch parent on children terminal");
}

/// Runs the child mutations in bounded parallel and returns the failure count.
/// Successes and failures are both tracked via child `ops_async_operation`
/// rows created by `admit_mutation`; this function is only responsible for
/// driving the fan-out and surfacing an overall failure tally so the parent
/// can settle.
async fn run_batch_reprocess_children(
    state: Arc<AppState>,
    operation: BatchDocumentOperation,
) -> usize {
    use futures::stream;

    let parent_id = operation.parent_id;
    let principal_id = operation.principal_id;
    let parallelism = operation.parallelism.max(1);
    let failure_count = stream::iter(operation.document_ids)
        .map(|document_id| {
            let state = state.clone();
            async move {
                match reprocess_single_document(
                    &state,
                    Some(parent_id),
                    principal_id,
                    None,
                    document_id,
                )
                .await
                {
                    Ok(_) => 0usize,
                    Err(error) => {
                        error!(%parent_id, %document_id, error = %error, "batch child rerun failed");
                        1usize
                    }
                }
            }
        })
        .buffer_unordered(parallelism)
        .collect::<Vec<usize>>()
        .await;
    failure_count.into_iter().sum()
}
