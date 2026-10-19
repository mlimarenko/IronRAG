use std::collections::BTreeMap;

use chrono::Utc;
use ironrag_contracts::documents::DocumentReadiness;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{
        ASYNC_OP_STATUS_PROCESSING, OpsAsyncOperation, OpsAsyncOperationProgress,
        OpsAsyncOperationStatus, OpsFailureDisposition, OpsLibraryState, OpsLibraryWarning,
    },
    domains::{
        content::{
            ContentDocumentPipelineJob, ContentDocumentSummary, ContentMutation,
            DocumentReadinessSummary, LibraryKnowledgeCoverage, revision_text_state_is_readable,
        },
        knowledge::{KnowledgeLibraryGeneration, StructuredDocumentRevision},
    },
    infra::knowledge_rows::KnowledgeRevisionRow,
    infra::repositories::{self, content_document_list_repository, ops_repository},
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateAsyncOperationCommand {
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub operation_kind: String,
    pub surface_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub status: String,
    pub subject_kind: String,
    pub subject_id: Option<Uuid>,
    /// When set, links this operation as a child of a parent batch op.
    /// Used by shared batch endpoints (batch-reprocess, …) so progress
    /// polling can aggregate child counts from a single parent id.
    pub parent_async_operation_id: Option<Uuid>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateAsyncOperationCommand {
    pub operation_id: Uuid,
    pub status: String,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
}

/// A terminal transition that also publishes what the operation produced.
#[derive(Debug, Clone)]
pub struct SettleAsyncOperationCommand {
    pub operation_id: Uuid,
    pub status: String,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
    /// `None` leaves the row's result null — a failed settle publishes its
    /// `failure_code`, not a payload.
    pub result_json: Option<serde_json::Value>,
}

#[derive(Clone, Default)]
pub struct OpsService;

#[derive(Debug, Clone)]
pub struct OpsLibraryStateSnapshot {
    pub state: OpsLibraryState,
    pub knowledge_generations: Vec<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Clone)]
pub struct OpsLibraryStateSnapshotWithWarnings {
    pub snapshot: OpsLibraryStateSnapshot,
    pub warnings: Vec<OpsLibraryWarning>,
}

/// Non-positional fan-out settle inputs shared by every writer of a batch
/// parent's terminal state. `guard` selects which persisted statuses the settle
/// may transition from; the three overrides mirror
/// [`ops_repository::settle_fan_out_operation_from_children`].
#[derive(Debug, Clone, Copy)]
pub struct FanOutParentSettle<'a> {
    pub fallback_failure_code: Option<&'a str>,
    pub childless_status: Option<&'a str>,
    pub detail: Option<&'a serde_json::Value>,
    pub guard: ops_repository::FanOutSettleGuard,
}

/// Source rows one document-readiness summary is derived from.
#[derive(Debug, Clone, Copy)]
pub struct DocumentReadinessInputs<'a> {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub effective_readiness_row: Option<&'a KnowledgeRevisionRow>,
    /// A scoped unpublished operational revision can prove failure, not readiness.
    pub operational_revision_failed: bool,
    pub prepared_revision: Option<&'a StructuredDocumentRevision>,
    pub latest_mutation: Option<&'a ContentMutation>,
    pub latest_job: Option<&'a ContentDocumentPipelineJob>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Counters that decide a library's degraded/rebuilding/processing state.
/// `vector_inventory_mismatch_count` and `graph_sparse_document_count` are
/// permanent plane holes (degradation); the queue counters are live work.
#[derive(Debug, Clone, Copy, Default)]
struct LibraryActivitySignals {
    queue_depth: i64,
    running_attempts: i64,
    failed_document_count: usize,
    in_flight_document_count: usize,
    vector_inventory_mismatch_count: usize,
    graph_sparse_document_count: usize,
    has_failed_attempts: bool,
    has_bundle_failures: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentKnowledgeCoverageState {
    pub processing_active: bool,
    pub failed: bool,
    pub readable: bool,
    pub graph_ready: bool,
    pub readiness_kind: DocumentReadiness,
    pub preparation_state: String,
    pub graph_coverage_kind: String,
    pub typed_fact_coverage: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DocumentKnowledgeSignals<'a> {
    pub processing_active: bool,
    pub hard_failure: bool,
    pub canceled_terminal: bool,
    pub revision_text_ready: bool,
    pub revision_graph_ready: bool,
    pub preparation_ready: bool,
    pub preparation_failed: bool,
    pub graph_failed: bool,
    pub observed_preparation_state: Option<&'a str>,
    pub block_count: Option<i32>,
    pub typed_fact_count: Option<i32>,
}

fn document_readiness_kind(
    signals: DocumentKnowledgeSignals<'_>,
    failed: bool,
    readable: bool,
    graph_ready: bool,
    graph_sparse: bool,
) -> DocumentReadiness {
    if failed {
        DocumentReadiness::Failed
    } else if signals.processing_active && readable {
        DocumentReadiness::Readable
    } else if signals.processing_active {
        DocumentReadiness::Processing
    } else if graph_ready {
        DocumentReadiness::GraphReady
    } else if graph_sparse {
        DocumentReadiness::GraphSparse
    } else if readable {
        DocumentReadiness::Readable
    } else {
        // No failed state, no work in flight, no graph coverage, no readable
        // revision: the document has no recorded work and none will start
        // without a new mutation. "Processing" would be a lie here (IRG-109).
        DocumentReadiness::Empty
    }
}

fn graph_coverage_kind(
    failed: bool,
    graph_ready: bool,
    graph_sparse: bool,
    processing_active: bool,
) -> &'static str {
    if failed {
        "failed"
    } else if graph_ready {
        "graph_ready"
    } else if graph_sparse {
        "graph_sparse"
    } else if processing_active {
        "processing"
    } else {
        "empty"
    }
}

fn preparation_state(signals: DocumentKnowledgeSignals<'_>, failed: bool) -> String {
    signals.observed_preparation_state.map_or_else(
        || {
            if failed {
                "failed"
            } else if signals.processing_active {
                "building"
            } else if signals.preparation_ready {
                "prepared"
            } else {
                "pending"
            }
            .to_string()
        },
        str::to_string,
    )
}

fn typed_fact_coverage(block_count: Option<i32>, typed_fact_count: Option<i32>) -> Option<f64> {
    block_count.map(|block_count| {
        if block_count <= 0 {
            0.0
        } else {
            (f64::from(typed_fact_count.unwrap_or_default()) / f64::from(block_count))
                .clamp(0.0, 1.0)
        }
    })
}

#[must_use]
pub(crate) fn classify_document_knowledge_signals(
    signals: DocumentKnowledgeSignals<'_>,
) -> DocumentKnowledgeCoverageState {
    let readable = signals.preparation_ready || signals.revision_text_ready;
    let graph_ready = signals.preparation_ready && signals.revision_graph_ready;
    let graph_sparse =
        readable && !graph_ready && (signals.preparation_ready || signals.revision_graph_ready);
    let failed = (signals.hard_failure || signals.canceled_terminal || signals.preparation_failed)
        && !readable;
    let graph_coverage_failed = failed || signals.graph_failed || signals.preparation_failed;

    DocumentKnowledgeCoverageState {
        processing_active: signals.processing_active,
        failed,
        readable,
        graph_ready,
        readiness_kind: document_readiness_kind(
            signals,
            failed,
            readable,
            graph_ready,
            graph_sparse,
        ),
        preparation_state: preparation_state(signals, failed),
        graph_coverage_kind: graph_coverage_kind(
            graph_coverage_failed,
            graph_ready,
            graph_sparse,
            signals.processing_active,
        )
        .to_string(),
        typed_fact_coverage: typed_fact_coverage(signals.block_count, signals.typed_fact_count),
    }
}

impl OpsService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn create_async_operation(
        &self,
        state: &AppState,
        command: CreateAsyncOperationCommand,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row = ops_repository::create_async_operation(
            &state.persistence.postgres,
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                operation_kind: &command.operation_kind,
                surface_kind: &command.surface_kind,
                requested_by_principal_id: command.requested_by_principal_id,
                status: &command.status,
                subject_kind: &command.subject_kind,
                subject_id: command.subject_id,
                parent_async_operation_id: command.parent_async_operation_id,
                completed_at: command.completed_at,
                failure_code: command.failure_code.as_deref(),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        map_async_operation_row(row)
    }

    /// Creates the tracking parent `ops_async_operation` for a fan-out over one
    /// library's documents or ingest jobs.
    ///
    /// The row starts `processing` with the library as its subject; the caller
    /// settles it when the detached fan-out finishes, so a UI polling the parent
    /// sees the whole action's outcome. The HTTP fan-outs (batch cancel, bulk
    /// ingest queue action) built this command verbatim in two places; keeping
    /// it here stops the two from drifting.
    pub async fn create_library_fan_out_operation(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        requested_by_principal_id: Option<Uuid>,
        operation_kind: &str,
    ) -> Result<OpsAsyncOperation, ApiError> {
        self.create_async_operation(
            state,
            CreateAsyncOperationCommand {
                workspace_id,
                library_id: Some(library_id),
                operation_kind: operation_kind.to_string(),
                surface_kind: "rest".to_string(),
                requested_by_principal_id,
                status: ASYNC_OP_STATUS_PROCESSING.to_string(),
                subject_kind: "library".to_string(),
                subject_id: Some(library_id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await
    }

    pub async fn update_async_operation(
        &self,
        state: &AppState,
        command: UpdateAsyncOperationCommand,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row = ops_repository::update_async_operation(
            &state.persistence.postgres,
            command.operation_id,
            &ops_repository::UpdateOpsAsyncOperation {
                status: &command.status,
                completed_at: command.completed_at,
                failure_code: command.failure_code.as_deref(),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("async_operation", command.operation_id))?;
        map_async_operation_row(row)
    }

    /// Settles a fan-out parent from its persisted children — the single point
    /// of truth for a batch parent's terminal state.
    ///
    /// The batch worker uses it on its normal path, the abnormal-exit guard
    /// uses it when the worker is aborted, and the operator repairs use it to
    /// re-derive parents an earlier release left stranded or misattributed.
    /// `Ok(None)` means the parent was already terminal, the guard did not
    /// match, or nothing decided its state — nothing was written. See
    /// [`ops_repository::settle_fan_out_operation_from_children`] for the
    /// derivation and the meaning of each argument.
    pub async fn settle_fan_out_parent_from_children(
        &self,
        state: &AppState,
        parent_id: Uuid,
        settle: FanOutParentSettle<'_>,
    ) -> Result<Option<OpsAsyncOperation>, ApiError> {
        let row = ops_repository::settle_fan_out_operation_from_children(
            &state.persistence.postgres,
            parent_id,
            settle.fallback_failure_code,
            settle.childless_status,
            settle.detail,
            settle.guard,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        row.map(map_async_operation_row).transpose()
    }

    /// Settles an operation and publishes what it produced, in one write.
    ///
    /// Distinct from `update_async_operation` because a result payload must be
    /// written once and never overwritten: the repository call is guarded on a
    /// still-in-flight status, so a second settle matches no row. `Ok(None)`
    /// therefore means the operation had already settled — the caller lost a
    /// race it must not paper over by writing anyway — while a missing row and
    /// an already-settled row are indistinguishable to the statement, which is
    /// why this returns the option instead of a not-found error.
    pub async fn settle_async_operation_with_result(
        &self,
        state: &AppState,
        command: SettleAsyncOperationCommand,
    ) -> Result<Option<OpsAsyncOperation>, ApiError> {
        let row = ops_repository::settle_async_operation_with_result(
            &state.persistence.postgres,
            command.operation_id,
            &command.status,
            command.completed_at,
            command.failure_code.as_deref(),
            command.result_json.as_ref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        row.map(map_async_operation_row).transpose()
    }

    pub async fn get_async_operation(
        &self,
        state: &AppState,
        operation_id: Uuid,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row =
            ops_repository::get_async_operation_by_id(&state.persistence.postgres, operation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("async_operation", operation_id))?;
        map_async_operation_row(row)
    }

    /// Aggregated child-operation counts for a parent batch `ops_async_operation`.
    /// Returns zero counts when the operation has no children (it is not a
    /// batch parent, or no children have been linked yet).
    pub async fn get_async_operation_progress(
        &self,
        state: &AppState,
        parent_id: Uuid,
    ) -> Result<OpsAsyncOperationProgress, ApiError> {
        let row =
            ops_repository::get_async_operation_progress(&state.persistence.postgres, parent_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(OpsAsyncOperationProgress {
            total: row.total,
            completed: row.completed,
            failed: row.failed,
            in_flight: row.in_flight,
        })
    }

    pub async fn get_latest_async_operation_by_subject(
        &self,
        state: &AppState,
        subject_kind: &str,
        subject_id: Uuid,
    ) -> Result<Option<OpsAsyncOperation>, ApiError> {
        let row = ops_repository::get_latest_async_operation_by_subject(
            &state.persistence.postgres,
            subject_kind,
            subject_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        row.map(map_async_operation_row).transpose()
    }

    pub async fn get_library_state_snapshot(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<OpsLibraryStateSnapshot, ApiError> {
        Ok(self.get_library_state_snapshot_with_warnings(state, library_id).await?.snapshot)
    }

    pub async fn get_library_state_snapshot_with_warnings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<OpsLibraryStateSnapshotWithWarnings, ApiError> {
        let (dashboard_snapshot, knowledge_generations) = tokio::try_join!(
            async {
                content_document_list_repository::load_library_dashboard_snapshot(
                    &state.persistence.postgres,
                    library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
            },
            state.application_services.knowledge.list_library_generations(state, library_id),
        )?;
        self.get_library_state_snapshot_with_warnings_from_dashboard_snapshot(
            state,
            library_id,
            &dashboard_snapshot,
            knowledge_generations,
        )
        .await
    }

    pub(crate) async fn get_library_state_snapshot_with_warnings_from_dashboard_snapshot(
        &self,
        state: &AppState,
        library_id: Uuid,
        dashboard_snapshot: &content_document_list_repository::LibraryDashboardSnapshot,
        mut knowledge_generations: Vec<KnowledgeLibraryGeneration>,
    ) -> Result<OpsLibraryStateSnapshotWithWarnings, ApiError> {
        knowledge_generations.sort_by(|left, right| {
            right.created_at.cmp(&left.created_at).then_with(|| right.id.cmp(&left.id))
        });
        let (facts, readiness, failed_attempts, bundle_failures) = tokio::try_join!(
            async {
                ops_repository::get_library_facts(&state.persistence.postgres, library_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or_else(|| ApiError::resource_not_found("library", library_id))
            },
            load_library_coverage_fast_from_dashboard_snapshot(
                state,
                library_id,
                knowledge_generations.first(),
                dashboard_snapshot,
            ),
            async {
                ops_repository::list_recent_failed_ingest_attempts(
                    &state.persistence.postgres,
                    library_id,
                    10,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
            },
            async {
                ops_repository::list_recent_bundle_assembly_failures(
                    &state.persistence.postgres,
                    library_id,
                    10,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
            },
        )?;
        let state = map_library_facts_from_aggregate(
            &facts,
            &readiness,
            &knowledge_generations,
            !failed_attempts.is_empty(),
            !bundle_failures.is_empty(),
        );
        let warnings = build_library_warnings_from_aggregate(
            library_id,
            &readiness,
            &failed_attempts,
            &bundle_failures,
        );
        Ok(OpsLibraryStateSnapshotWithWarnings {
            snapshot: OpsLibraryStateSnapshot { state, knowledge_generations },
            warnings,
        })
    }

    pub async fn list_library_warnings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<OpsLibraryWarning>, ApiError> {
        let readiness = load_library_coverage_fast(state, library_id, None).await?;
        let failed_attempts = ops_repository::list_recent_failed_ingest_attempts(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let bundle_failures = ops_repository::list_recent_bundle_assembly_failures(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(build_library_warnings_from_aggregate(
            library_id,
            &readiness,
            &failed_attempts,
            &bundle_failures,
        ))
    }

    #[must_use]
    pub fn classify_document_knowledge_state(
        &self,
        effective_readiness_row: Option<&KnowledgeRevisionRow>,
        prepared_revision: Option<&StructuredDocumentRevision>,
        latest_mutation: Option<&ContentMutation>,
        latest_job: Option<&ContentDocumentPipelineJob>,
        operational_revision_failed: bool,
    ) -> DocumentKnowledgeCoverageState {
        let processing_active = latest_job
            .as_ref()
            .is_some_and(|job| matches!(job.queue_state.as_str(), "queued" | "leased"))
            || latest_mutation.as_ref().is_some_and(|mutation| {
                matches!(mutation.mutation_state.as_str(), "accepted" | "running")
            });
        let hard_failure = operational_revision_failed
            || latest_job.as_ref().is_some_and(|job| job.queue_state == "failed")
            || latest_mutation.as_ref().is_some_and(|mutation| mutation.mutation_state == "failed")
            || effective_readiness_row.as_ref().is_some_and(|revision| {
                revision.text_state == "failed" || revision.vector_state == "failed"
            })
            || prepared_revision
                .as_ref()
                .is_some_and(|revision| revision.preparation_state == "failed");
        let canceled_terminal =
            latest_job.as_ref().is_some_and(|job| job.queue_state == "canceled")
                || latest_mutation
                    .as_ref()
                    .is_some_and(|mutation| mutation.mutation_state == "canceled");
        let revision_text_ready = effective_readiness_row
            .as_ref()
            .is_some_and(|revision| revision_text_state_is_readable(&revision.text_state));
        let revision_graph_ready = effective_readiness_row
            .as_ref()
            .is_some_and(|revision| revision.graph_state == "ready");
        let graph_failed = effective_readiness_row
            .as_ref()
            .is_some_and(|revision| revision.graph_state == "failed");
        let preparation_ready = prepared_revision
            .as_ref()
            .is_some_and(|revision| revision.preparation_state == "prepared");
        classify_document_knowledge_signals(DocumentKnowledgeSignals {
            processing_active,
            hard_failure,
            canceled_terminal,
            revision_text_ready,
            revision_graph_ready,
            preparation_ready,
            preparation_failed: prepared_revision
                .as_ref()
                .is_some_and(|revision| revision.preparation_state == "failed"),
            graph_failed,
            observed_preparation_state: prepared_revision
                .as_ref()
                .map(|revision| revision.preparation_state.as_str()),
            block_count: prepared_revision.as_ref().map(|revision| revision.block_count),
            typed_fact_count: prepared_revision.as_ref().map(|revision| revision.typed_fact_count),
        })
    }

    #[must_use]
    pub fn derive_document_readiness_summary(
        &self,
        state: &AppState,
        inputs: DocumentReadinessInputs<'_>,
    ) -> DocumentReadinessSummary {
        let classification = self.classify_document_knowledge_state(
            inputs.effective_readiness_row,
            inputs.prepared_revision,
            inputs.latest_mutation,
            inputs.latest_job,
            inputs.operational_revision_failed,
        );
        let now = Utc::now();
        let activity_status =
            state.bulk_ingest_hardening_services.ingest_activity.derive_document_activity(
                inputs.latest_mutation,
                inputs.latest_job,
                classification.readable,
                classification.graph_ready,
                now,
            );
        let stalled_reason =
            state.bulk_ingest_hardening_services.ingest_activity.document_stalled_reason(
                inputs.latest_mutation,
                inputs.latest_job,
                classification.readable,
                classification.graph_ready,
                now,
            );
        let updated_at = [
            Some(inputs.created_at),
            inputs.latest_job.and_then(|job| job.completed_at.or(Some(job.queued_at))),
            inputs.latest_mutation.map(|mutation| mutation.requested_at),
            inputs.effective_readiness_row.and_then(|revision| revision.text_readable_at),
            inputs.effective_readiness_row.and_then(|revision| revision.vector_ready_at),
            inputs.effective_readiness_row.and_then(|revision| revision.graph_ready_at),
            inputs.prepared_revision.map(|revision| revision.prepared_at),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(inputs.created_at);

        DocumentReadinessSummary {
            document_id: inputs.document_id,
            active_revision_id: inputs.active_revision_id,
            readiness_kind: classification.readiness_kind,
            activity_status,
            stalled_reason,
            preparation_state: classification.preparation_state,
            graph_coverage_kind: classification.graph_coverage_kind,
            typed_fact_coverage: classification.typed_fact_coverage,
            last_mutation_id: inputs.latest_mutation.map(|mutation| mutation.id),
            last_job_stage: inputs.latest_job.and_then(|job| job.current_stage.clone()),
            updated_at,
        }
    }

    #[must_use]
    pub fn derive_library_knowledge_coverage(
        &self,
        library_id: Uuid,
        summaries: &[ContentDocumentSummary],
        last_generation_id: Option<Uuid>,
    ) -> LibraryKnowledgeCoverage {
        let mut document_counts_by_readiness = BTreeMap::<String, i64>::new();
        let mut graph_ready_document_count = 0_i64;
        let mut graph_sparse_document_count = 0_i64;
        let mut typed_fact_document_count = 0_i64;
        let mut updated_at = summaries
            .iter()
            .filter_map(|summary| summary.readiness_summary.as_ref().map(|item| item.updated_at))
            .max()
            .unwrap_or_else(Utc::now);

        for summary in
            summaries.iter().filter(|summary| summary.document.document_state != "deleted")
        {
            let Some(readiness) = summary.readiness_summary.as_ref() else {
                continue;
            };
            *document_counts_by_readiness
                .entry(readiness.readiness_kind.as_str().to_string())
                .or_default() += 1;
            match readiness.graph_coverage_kind.as_str() {
                "graph_ready" => graph_ready_document_count += 1,
                "graph_sparse" => graph_sparse_document_count += 1,
                _ => {}
            }
            if readiness.typed_fact_coverage.unwrap_or_default() > 0.0
                || summary
                    .prepared_revision
                    .as_ref()
                    .is_some_and(|revision| revision.typed_fact_count > 0)
            {
                typed_fact_document_count += 1;
            }
            updated_at = updated_at.max(readiness.updated_at);
        }

        LibraryKnowledgeCoverage {
            library_id,
            document_counts_by_readiness,
            graph_ready_document_count,
            graph_sparse_document_count,
            typed_fact_document_count,
            last_generation_id,
            updated_at,
        }
    }
}

/// Fast O(1) library readiness snapshot used by the dashboard and warnings
/// surfaces. Replaces the previous O(N) `list_documents` + N+1 prefetch
/// storm that ran per-document knowledge-store + Postgres fan-outs — on a 5k-doc
/// library the old path took 7+ seconds and gated the query execution
/// context. This path is two Postgres reads plus one bounded knowledge aggregate
/// for vector-inventory drift; it stays off the query-turn hot path.
pub struct LibraryCoverageFast {
    /// Authoritative per-library metrics row produced by
    /// `aggregate_library_document_metrics`. Replaces the retired
    /// `LibraryDocumentReadinessAggregate` — every downstream consumer
    /// reads from this single source so `/ops/libraries/{id}` and
    /// `/ops/libraries/{id}/dashboard` can never disagree on the
    /// numbers.
    pub metrics: ironrag_contracts::documents::LibraryDocumentMetrics,
    pub graph_snapshot: Option<repositories::RuntimeGraphSnapshotRow>,
    pub latest_generation_id: Option<Uuid>,
    pub vector_inventory_mismatch_count: i64,
    /// When the oldest claimable job in this library was queued, if any.
    pub oldest_available_queued_job_at: Option<chrono::DateTime<Utc>>,
}

/// How long a library may sit with claimable work and no running attempt
/// before the queue is reported as stalled rather than busy. Long enough that
/// a worker between two claims never trips it.
const STALLED_QUEUE_THRESHOLD_MINUTES: i64 = 15;

pub async fn load_library_coverage_fast(
    state: &AppState,
    library_id: Uuid,
    latest_generation: Option<&KnowledgeLibraryGeneration>,
) -> Result<LibraryCoverageFast, ApiError> {
    let dashboard_snapshot = content_document_list_repository::load_library_dashboard_snapshot(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    load_library_coverage_fast_from_dashboard_snapshot(
        state,
        library_id,
        latest_generation,
        &dashboard_snapshot,
    )
    .await
}

async fn load_library_coverage_fast_from_dashboard_snapshot(
    state: &AppState,
    library_id: Uuid,
    latest_generation: Option<&KnowledgeLibraryGeneration>,
    dashboard_snapshot: &content_document_list_repository::LibraryDashboardSnapshot,
) -> Result<LibraryCoverageFast, ApiError> {
    // The vector inventory gap is owned by the document store's single-source
    // `list_documents_missing_chunk_vectors` — the `vector_index_gap` notice
    // count and the documents-list `indexing=vector_gap` filter must resolve the
    // same set — so it stays off the dashboard transaction. The stalled-queue age
    // is read inside that transaction (`LibraryDashboardSnapshot`), so the
    // coverage notice can never combine it with counts from another MVCC instant.
    let vector_inventory_mismatch_count = state
        .document_store
        .list_documents_missing_chunk_vectors(library_id)
        .await
        .map(|documents| i64::try_from(documents.len()).unwrap_or(i64::MAX))
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(LibraryCoverageFast {
        metrics: dashboard_snapshot.metrics.clone(),
        graph_snapshot: dashboard_snapshot.graph_snapshot.clone(),
        latest_generation_id: latest_generation.map(|generation| generation.id),
        vector_inventory_mismatch_count,
        oldest_available_queued_job_at: dashboard_snapshot.oldest_available_queued_job_at,
    })
}

fn map_library_facts_from_aggregate(
    row: &ops_repository::OpsLibraryFactsRow,
    coverage: &LibraryCoverageFast,
    knowledge_generations: &[KnowledgeLibraryGeneration],
    has_failed_attempts: bool,
    has_bundle_failures: bool,
) -> OpsLibraryState {
    let latest_knowledge_generation = knowledge_generations.first();
    let readable_document_count = coverage.metrics.ready;
    let failed_document_count = coverage.metrics.failed;
    // Documents whose latest mutation is still in flight. The status aggregate
    // counts both `processing` (running attempt) and `queued` (enqueued, not
    // yet leased) as work-in-flight; merged here, they are the library's live
    // work and nothing else is.
    let in_flight_document_count =
        usize::try_from(coverage.metrics.processing + coverage.metrics.queued).unwrap_or(0);

    OpsLibraryState {
        library_id: row.library_id,
        queue_depth: row.queue_depth,
        running_attempts: row.running_attempts,
        readable_document_count,
        failed_document_count,
        degraded_state: derive_degraded_state(LibraryActivitySignals {
            queue_depth: row.queue_depth,
            running_attempts: row.running_attempts,
            failed_document_count: usize::try_from(failed_document_count).unwrap_or(usize::MAX),
            in_flight_document_count,
            vector_inventory_mismatch_count: usize::try_from(
                coverage.vector_inventory_mismatch_count,
            )
            .unwrap_or(usize::MAX),
            graph_sparse_document_count: usize::try_from(coverage.metrics.graph_sparse)
                .unwrap_or(usize::MAX),
            has_failed_attempts,
            has_bundle_failures,
        }),
        latest_knowledge_generation_id: latest_knowledge_generation.map(|generation| generation.id),
        knowledge_generation_state: latest_knowledge_generation
            .map(|generation| generation.generation_state.clone()),
        last_recomputed_at: row.last_recomputed_at,
    }
}

fn build_library_warnings_from_aggregate(
    library_id: Uuid,
    coverage: &LibraryCoverageFast,
    failed_attempts: &[ops_repository::OpsLibraryFailureRow],
    bundle_failures: &[ops_repository::OpsLibraryFailureRow],
) -> Vec<OpsLibraryWarning> {
    let mut warnings = Vec::new();

    // Work in flight is normal operation, not an operator warning: a document
    // whose latest mutation is `processing` or `queued` is being caught up by
    // the queue that owns it. The retired `stale_vectors` / `stale_relations`
    // notices fired off exactly that `processing + queued` signal, so a healthy
    // busy library reported lag it did not have and pointed the operator at the
    // in-flight list to reprocess work already running. The genuine holes that
    // do not close on their own keep their own notices below —
    // `vector_index_gap`, `graph_coverage_gap` and the stalled
    // `queue_not_progressing` — and are the only conditions that reach the
    // operator from here.

    // Ready documents that hold chunks and no chunk vectors. This is not the
    // same condition as the lag above and does not share its recovery: nothing
    // is in flight that will catch it up, so the operator has to reprocess the
    // affected documents. Reported as an error for the same reason
    // `derive_degraded_state` calls the library degraded rather than
    // rebuilding — the gap does not close on its own.
    if coverage.vector_inventory_mismatch_count > 0 {
        warnings.push(derived_warning(library_id, "vector_index_gap", "error", Utc::now()));
    }

    // Ready documents whose graph extraction failed. `graph_state='failed'`
    // ready docs fold into the O(1) `graph_sparse` counter, so this gap does not
    // close on its own either: nothing is in flight that will catch it up and
    // the operator has to reprocess the affected documents. Same rationale as
    // `vector_index_gap`, but a warning because the text is still readable.
    if coverage.metrics.graph_sparse > 0 {
        warnings.push(derived_warning(library_id, "graph_coverage_gap", "warning", Utc::now()));
    }

    // Claimable work with no running attempt is a stalled queue, not progress.
    // Reported as an error because it never clears on its own: the library
    // keeps accepting documents while none of them can ever be ingested.
    if let Some(queued_since) = coverage.oldest_available_queued_job_at
        && coverage.metrics.processing == 0
        && coverage.metrics.queued > 0
        && Utc::now().signed_duration_since(queued_since)
            > chrono::Duration::minutes(STALLED_QUEUE_THRESHOLD_MINUTES)
    {
        warnings.push(derived_warning(library_id, "queue_not_progressing", "error", queued_since));
    }

    if let Some(latest_failure) = failed_attempts.first() {
        warnings.push(derived_warning(
            library_id,
            "failed_rebuilds",
            "error",
            latest_failure.created_at,
        ));
    }

    if let Some(latest_failure) = bundle_failures.first() {
        warnings.push(derived_warning(
            library_id,
            "bundle_assembly_failures",
            "error",
            latest_failure.created_at,
        ));
    }

    warnings.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.warning_kind.cmp(&right.warning_kind))
    });
    warnings
}

/// The one row-to-domain mapping for `ops_async_operation`.
///
/// `pub(crate)` because ingest reads the same rows for its job listing; it used
/// to carry a byte-identical copy, which is exactly how a column added to the
/// row reaches one reader and not the other.
pub(crate) fn map_async_operation_row(
    row: ops_repository::OpsAsyncOperationRow,
) -> Result<OpsAsyncOperation, ApiError> {
    let status = OpsAsyncOperationStatus::from_db(&row.status)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    // Forward compatibility: the column is plain `text`, so a newer binary or a
    // manual edit can leave a value this binary does not know. That is not a
    // reason to fail the whole read — the operation still exists and its code,
    // status and result are readable. It is surfaced as absent (the same honest
    // `null` an untouched operation carries) and logged loudly instead. Writes
    // stay strict: they only ever send `OpsFailureDisposition::as_str`.
    let failure_disposition = row.failure_disposition.as_deref().and_then(|value| {
        OpsFailureDisposition::from_db(value)
            .inspect_err(|error| {
                tracing::warn!(
                    operation_id = %row.id,
                    error = %error,
                    "encountered unknown async operation failure disposition; surfacing null"
                );
            })
            .ok()
    });
    Ok(OpsAsyncOperation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        status,
        surface_kind: Some(row.surface_kind),
        subject_kind: Some(row.subject_kind),
        subject_id: row.subject_id,
        parent_async_operation_id: row.parent_async_operation_id,
        failure_code: row.failure_code,
        failure_disposition,
        created_at: row.created_at,
        completed_at: row.completed_at,
        result_json: row.result_json,
    })
}

/// Each input here has exactly one owner, and the branches keep them apart.
///
/// Live work is owned by the ingest queue: `in_flight_document_count` counts
/// documents whose latest mutation is leased or waiting, `queue_depth` and
/// `running_attempts` count the queue rows behind them. Only those may claim a
/// rebuild is under way.
///
/// `vector_inventory_mismatch_count` is owned by the vector plane and says
/// something else entirely: revisions published as vector-ready that carry no
/// chunk vectors. That hole does not close on its own and no worker is working
/// on it, so it is degradation, not progress. Summing it into the rebuild
/// signal is what made an idle library report "rebuilding" for good.
///
/// `graph_sparse_document_count` is the same kind of hole in the graph plane:
/// ready documents whose graph extraction failed (`graph_state='failed'` folds
/// into the sparse counter). No worker closes it, so it is degradation too.
fn derive_degraded_state(signals: LibraryActivitySignals) -> String {
    if signals.failed_document_count > 0
        || signals.has_failed_attempts
        || signals.has_bundle_failures
        || signals.vector_inventory_mismatch_count > 0
        || signals.graph_sparse_document_count > 0
    {
        "degraded".to_string()
    } else if signals.in_flight_document_count > 0 {
        "rebuilding".to_string()
    } else if signals.queue_depth > 0 || signals.running_attempts > 0 {
        "processing".to_string()
    } else {
        "healthy".to_string()
    }
}

fn derived_warning(
    library_id: Uuid,
    warning_kind: &str,
    severity: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> OpsLibraryWarning {
    let warning_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("ops-warning:{library_id}:{warning_kind}").as_bytes(),
    );
    OpsLibraryWarning {
        id: warning_id,
        library_id,
        warning_kind: warning_kind.to_string(),
        severity: severity.to_string(),
        created_at,
        resolved_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DocumentKnowledgeSignals, LibraryActivitySignals, LibraryCoverageFast,
        STALLED_QUEUE_THRESHOLD_MINUTES, build_library_warnings_from_aggregate,
        classify_document_knowledge_signals, derive_degraded_state,
        map_library_facts_from_aggregate,
    };
    use crate::domains::ops::OpsLibraryWarning;
    use crate::infra::repositories::ops_repository;
    use chrono::Utc;
    use ironrag_contracts::documents::{DocumentReadiness, LibraryDocumentMetrics};
    use uuid::Uuid;

    fn coverage_with_processing(processing_count: i64) -> LibraryCoverageFast {
        coverage_with_processing_and_vector_mismatch(processing_count, 0)
    }

    fn coverage_with_processing_and_vector_mismatch(
        processing_count: i64,
        vector_inventory_mismatch_count: i64,
    ) -> LibraryCoverageFast {
        LibraryCoverageFast {
            metrics: LibraryDocumentMetrics {
                total: processing_count.max(1),
                ready: 0,
                processing: processing_count,
                queued: 0,
                failed: 0,
                canceled: 0,
                empty: 0,
                graph_ready: 0,
                graph_sparse: 0,
                recomputed_at: Utc::now(),
            },
            graph_snapshot: None,
            latest_generation_id: None,
            vector_inventory_mismatch_count,
            oldest_available_queued_job_at: None,
        }
    }

    #[test]
    fn library_state_does_not_fold_canceled_documents_into_failed_count() {
        let coverage = LibraryCoverageFast {
            metrics: LibraryDocumentMetrics {
                total: 2,
                ready: 0,
                processing: 0,
                queued: 0,
                failed: 0,
                canceled: 2,
                empty: 0,
                graph_ready: 0,
                graph_sparse: 0,
                recomputed_at: Utc::now(),
            },
            graph_snapshot: None,
            latest_generation_id: None,
            vector_inventory_mismatch_count: 0,
            oldest_available_queued_job_at: None,
        };
        let row = ops_repository::OpsLibraryFactsRow {
            library_id: Uuid::now_v7(),
            queue_depth: 0,
            running_attempts: 0,
            degraded_state: "ready".to_string(),
            last_recomputed_at: Utc::now(),
        };

        let state = map_library_facts_from_aggregate(&row, &coverage, &[], false, false);

        assert_eq!(state.failed_document_count, 0);
        assert_eq!(state.degraded_state, "healthy");
    }

    #[test]
    fn classifier_keeps_readable_revision_visible_after_canceled_job() {
        let state = classify_document_knowledge_signals(DocumentKnowledgeSignals {
            canceled_terminal: true,
            revision_text_ready: true,
            ..Default::default()
        });

        assert!(!state.failed);
        assert!(state.readable);
        assert_eq!(state.readiness_kind, DocumentReadiness::Readable);
        assert_eq!(state.graph_coverage_kind, "empty");
    }

    #[test]
    fn classifier_reports_empty_for_document_without_mutation_job_or_revision() {
        let state = classify_document_knowledge_signals(DocumentKnowledgeSignals::default());

        assert!(!state.failed);
        assert!(!state.readable);
        assert!(!state.processing_active);
        assert_eq!(state.readiness_kind, DocumentReadiness::Empty);
        assert_eq!(state.graph_coverage_kind, "empty");
        assert_eq!(state.preparation_state, "pending");
    }

    #[test]
    fn classifier_reports_graph_failure_without_hiding_readable_text() {
        let state = classify_document_knowledge_signals(DocumentKnowledgeSignals {
            revision_text_ready: true,
            graph_failed: true,
            ..Default::default()
        });

        assert!(!state.failed);
        assert!(state.readable);
        assert_eq!(state.readiness_kind, DocumentReadiness::Readable);
        assert_eq!(state.graph_coverage_kind, "failed");
    }

    #[test]
    fn classifier_fails_when_terminal_state_has_no_readable_artifact() {
        let state = classify_document_knowledge_signals(DocumentKnowledgeSignals {
            canceled_terminal: true,
            ..Default::default()
        });

        assert!(state.failed);
        assert!(!state.readable);
        assert_eq!(state.readiness_kind, DocumentReadiness::Failed);
    }

    #[test]
    fn classifier_keeps_readable_revision_visible_after_canceled_mutation() {
        let state = classify_document_knowledge_signals(DocumentKnowledgeSignals {
            canceled_terminal: true,
            revision_text_ready: true,
            ..Default::default()
        });

        assert!(!state.failed);
        assert!(state.readable);
        assert_eq!(state.readiness_kind, DocumentReadiness::Readable);
    }

    #[test]
    fn derive_degraded_state_reports_healthy_when_idle_without_active_rebuilds() {
        let degraded_state = derive_degraded_state(LibraryActivitySignals::default());

        assert_eq!(degraded_state, "healthy");
    }

    /// A vector inventory gap is a permanent hole in the vector plane, not work
    /// in progress: no job is queued, no attempt is running, and nothing will
    /// close it on its own. Announcing it as a rebuild made the dashboard claim
    /// background work on a library the queue itself reported idle.
    #[test]
    fn library_state_does_not_announce_a_rebuild_for_an_idle_vector_inventory_gap() {
        let coverage = coverage_with_processing_and_vector_mismatch(0, 609);
        let row = ops_repository::OpsLibraryFactsRow {
            library_id: Uuid::now_v7(),
            queue_depth: 0,
            running_attempts: 0,
            degraded_state: "healthy".to_string(),
            last_recomputed_at: Utc::now(),
        };

        let state = map_library_facts_from_aggregate(&row, &coverage, &[], false, false);

        assert_ne!(
            state.degraded_state, "rebuilding",
            "an empty queue with no running attempt is not a live rebuild, whatever the vector inventory is missing",
        );
    }

    /// A library with claimable work and nothing running.
    fn coverage_with_stalled_queue(queued_for_minutes: i64) -> LibraryCoverageFast {
        let mut coverage = coverage_with_processing(0);
        coverage.metrics.total = 1;
        coverage.metrics.queued = 1;
        coverage.oldest_available_queued_job_at =
            Some(Utc::now() - chrono::Duration::minutes(queued_for_minutes));
        coverage
    }

    #[test]
    fn build_library_warnings_reports_a_queue_that_stopped_moving() {
        let coverage = coverage_with_stalled_queue(STALLED_QUEUE_THRESHOLD_MINUTES + 5);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);
        let stalled = warnings
            .iter()
            .find(|warning| warning.warning_kind == "queue_not_progressing")
            .expect("a queue with claimable work and no running attempt must be reported");
        assert_eq!(stalled.severity, "error");
    }

    #[test]
    fn build_library_warnings_tolerates_a_queue_between_claims() {
        let coverage = coverage_with_stalled_queue(STALLED_QUEUE_THRESHOLD_MINUTES - 5);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);
        assert!(
            !warnings.iter().any(|warning| warning.warning_kind == "queue_not_progressing"),
            "a worker between two claims must not raise a stalled-queue alarm",
        );
    }

    #[test]
    fn build_library_warnings_ignores_idle_library() {
        let coverage = coverage_with_processing(0);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);
        assert!(warnings.is_empty());
    }

    /// Work in flight over a fully graph-ready library is normal operation:
    /// the queue that owns those documents is catching them up. The retired
    /// `stale_vectors` / `stale_relations` notices fired off `processing +
    /// queued` alone, so a healthy busy library with a complete graph reported
    /// lag it did not have and pointed the operator at the in-flight list. A
    /// live backend screenshot (ready 43, processing 8, queued 17, graph ring
    /// 100%) raised exactly those false notices.
    #[test]
    fn build_library_warnings_does_not_report_lag_for_in_flight_ready_graph() {
        let mut coverage = coverage_with_processing(3);
        coverage.metrics.total = 10;
        coverage.metrics.queued = 2;
        coverage.metrics.ready = 5;
        coverage.metrics.graph_ready = 5;
        coverage.metrics.graph_sparse = 0;

        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);

        assert!(
            warnings.is_empty(),
            "in-flight work with a complete graph and no real gap is not a warning: {warnings:?}",
        );
    }

    /// A library whose ready documents hold chunks and no chunk vectors is not
    /// "behind" anything — nothing is running that will catch it up. Reporting
    /// it as a lag notice sent the operator to the queued-and-processing
    /// list, which is empty in exactly this case, so the notice named a real
    /// condition and then pointed away from it.
    #[test]
    fn build_library_warnings_reports_vector_gap_as_its_own_notice() {
        let coverage = coverage_with_processing_and_vector_mismatch(0, 2);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);

        let kinds: Vec<&str> =
            warnings.iter().map(|warning| warning.warning_kind.as_str()).collect();
        assert_eq!(kinds, vec!["vector_index_gap"]);
        assert!(warnings.iter().all(|warning: &OpsLibraryWarning| warning.severity == "error"));
    }

    /// A ready document whose graph extraction failed is a permanent hole in the
    /// graph plane: no worker closes it, so it is degradation, not a rebuild.
    #[test]
    fn derive_degraded_state_treats_a_graph_coverage_gap_as_degradation() {
        let degraded_state = derive_degraded_state(LibraryActivitySignals {
            graph_sparse_document_count: 3,
            ..LibraryActivitySignals::default()
        });

        assert_eq!(
            degraded_state, "degraded",
            "a failed graph does not close on its own, so the library is degraded, not healthy"
        );
    }

    #[test]
    fn build_library_warnings_reports_failed_graphs_as_a_coverage_gap() {
        let mut coverage = coverage_with_processing(0);
        // `graph_state='failed'` ready documents fold into the O(1) sparse
        // counter rather than a separate field.
        coverage.metrics.graph_sparse = 3;
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);

        let gap = warnings
            .iter()
            .find(|warning| warning.warning_kind == "graph_coverage_gap")
            .expect("a library with failed graphs must warn about the coverage gap");
        assert_eq!(gap.severity, "warning");
    }
}
