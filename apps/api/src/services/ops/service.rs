use std::collections::BTreeMap;

use chrono::Utc;
use ironrag_contracts::documents::DocumentReadiness;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{
        OpsAsyncOperation, OpsAsyncOperationProgress, OpsAsyncOperationStatus, OpsLibraryState,
        OpsLibraryWarning,
    },
    domains::{
        content::{
            ContentDocumentPipelineJob, ContentDocumentSummary, ContentMutation,
            DocumentReadinessSummary, LibraryKnowledgeCoverage, revision_text_state_is_readable,
        },
        knowledge::{KnowledgeLibraryGeneration, StructuredDocumentRevision},
    },
    infra::knowledge_rows::KnowledgeRevisionRow,
    infra::repositories::{self, content_repository, ops_repository},
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
    /// Used by canonical batch endpoints (batch-reprocess, …) so progress
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

    let readiness_kind = if failed {
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
        DocumentReadiness::Processing
    };
    let graph_coverage_kind = if graph_coverage_failed {
        "failed"
    } else if graph_ready {
        "graph_ready"
    } else if graph_sparse {
        "graph_sparse"
    } else {
        "processing"
    };
    let preparation_state =
        signals.observed_preparation_state.map(str::to_string).unwrap_or_else(|| {
            if failed {
                "failed".to_string()
            } else if signals.processing_active {
                "building".to_string()
            } else if signals.preparation_ready {
                "prepared".to_string()
            } else {
                "pending".to_string()
            }
        });
    let typed_fact_coverage = signals.block_count.map(|block_count| {
        if block_count <= 0 {
            0.0
        } else {
            let typed_fact_count = signals.typed_fact_count.unwrap_or_default();
            (f64::from(typed_fact_count) / f64::from(block_count)).clamp(0.0, 1.0)
        }
    });

    DocumentKnowledgeCoverageState {
        processing_active: signals.processing_active,
        failed,
        readable,
        graph_ready,
        readiness_kind,
        preparation_state,
        graph_coverage_kind: graph_coverage_kind.to_string(),
        typed_fact_coverage,
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
        let facts = ops_repository::get_library_facts(&state.persistence.postgres, library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let mut knowledge_generations =
            state.canonical_services.knowledge.list_library_generations(state, library_id).await?;
        knowledge_generations.sort_by(|left, right| {
            right.created_at.cmp(&left.created_at).then_with(|| right.id.cmp(&left.id))
        });
        let readiness =
            load_library_coverage_fast(state, library_id, knowledge_generations.first()).await?;
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
    ) -> DocumentKnowledgeCoverageState {
        let processing_active = latest_job
            .as_ref()
            .is_some_and(|job| matches!(job.queue_state.as_str(), "queued" | "leased"))
            || latest_mutation.as_ref().is_some_and(|mutation| {
                matches!(mutation.mutation_state.as_str(), "accepted" | "running")
            });
        let hard_failure = latest_job.as_ref().is_some_and(|job| job.queue_state == "failed")
            || latest_mutation.as_ref().is_some_and(|mutation| {
                matches!(mutation.mutation_state.as_str(), "failed" | "conflicted")
            })
            || effective_readiness_row.as_ref().is_some_and(|revision| {
                matches!(revision.text_state.as_str(), "failed" | "unavailable")
                    || revision.vector_state == "failed"
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
        let revision_graph_ready = effective_readiness_row.as_ref().is_some_and(|revision| {
            matches!(revision.graph_state.as_str(), "ready" | "graph_ready")
        });
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

    pub fn derive_document_readiness_summary(
        &self,
        state: &AppState,
        document_id: Uuid,
        active_revision_id: Option<Uuid>,
        effective_readiness_row: Option<&KnowledgeRevisionRow>,
        prepared_revision: Option<&StructuredDocumentRevision>,
        latest_mutation: Option<&ContentMutation>,
        latest_job: Option<&ContentDocumentPipelineJob>,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> DocumentReadinessSummary {
        let classification = self.classify_document_knowledge_state(
            effective_readiness_row,
            prepared_revision,
            latest_mutation,
            latest_job,
        );
        let now = Utc::now();
        let activity_status =
            state.bulk_ingest_hardening_services.ingest_activity.derive_document_activity(
                latest_mutation,
                latest_job,
                classification.readable,
                classification.graph_ready,
                now,
            );
        let stalled_reason =
            state.bulk_ingest_hardening_services.ingest_activity.document_stalled_reason(
                latest_mutation,
                latest_job,
                classification.readable,
                classification.graph_ready,
                now,
            );
        let updated_at = [
            Some(created_at),
            latest_job.and_then(|job| job.completed_at.or(Some(job.queued_at))),
            latest_mutation.map(|mutation| mutation.requested_at),
            effective_readiness_row.and_then(|revision| revision.text_readable_at),
            effective_readiness_row.and_then(|revision| revision.vector_ready_at),
            effective_readiness_row.and_then(|revision| revision.graph_ready_at),
            prepared_revision.map(|revision| revision.prepared_at),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(created_at);

        DocumentReadinessSummary {
            document_id,
            active_revision_id,
            readiness_kind: classification.readiness_kind,
            activity_status,
            stalled_reason,
            preparation_state: classification.preparation_state,
            graph_coverage_kind: classification.graph_coverage_kind,
            typed_fact_coverage: classification.typed_fact_coverage,
            last_mutation_id: latest_mutation.map(|mutation| mutation.id),
            last_job_stage: latest_job.and_then(|job| job.current_stage.clone()),
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
    /// Canonical per-library metrics row produced by
    /// `aggregate_library_document_metrics`. Replaces the retired
    /// `LibraryDocumentReadinessAggregate` — every downstream consumer
    /// reads from this single source so `/ops/libraries/{id}` and
    /// `/ops/libraries/{id}/dashboard` can never disagree on the
    /// numbers.
    pub metrics: ironrag_contracts::documents::LibraryDocumentMetrics,
    pub graph_snapshot: Option<repositories::RuntimeGraphSnapshotRow>,
    pub latest_generation_id: Option<Uuid>,
    pub vector_inventory_mismatch_count: i64,
}

pub async fn load_library_coverage_fast(
    state: &AppState,
    library_id: Uuid,
    latest_generation: Option<&KnowledgeLibraryGeneration>,
) -> Result<LibraryCoverageFast, ApiError> {
    let metrics = content_repository::aggregate_library_document_metrics(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let (graph_snapshot, vector_inventory_mismatch_count) = tokio::try_join!(
        async {
            repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
        },
        async {
            state
                .document_store
                .count_vector_ready_revisions_missing_chunk_vectors(library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
        },
    )?;
    Ok(LibraryCoverageFast {
        metrics,
        graph_snapshot,
        latest_generation_id: latest_generation.map(|generation| generation.id),
        vector_inventory_mismatch_count,
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
    // Approximation: "rebuilding" docs are those whose latest mutation is
    // still in-flight. The canonical metrics aggregate counts both
    // `processing` (running attempt) and `queued` (enqueued, not yet
    // leased) as work-in-flight; we merge them here to feed the
    // healthy/rebuilding/processing banner and the derived
    // degraded_state heuristic.
    let in_flight = coverage.metrics.processing + coverage.metrics.queued;
    let processing_count_usize = usize::try_from(in_flight).unwrap_or(0);
    let stale_vector_count = processing_count_usize.saturating_add(
        usize::try_from(coverage.vector_inventory_mismatch_count).unwrap_or(usize::MAX),
    );
    let stale_relation_count = processing_count_usize;

    OpsLibraryState {
        library_id: row.library_id,
        queue_depth: row.queue_depth,
        running_attempts: row.running_attempts,
        readable_document_count,
        failed_document_count,
        degraded_state: derive_degraded_state(
            row.queue_depth,
            row.running_attempts,
            usize::try_from(failed_document_count).unwrap_or(usize::MAX),
            stale_vector_count,
            stale_relation_count,
            has_failed_attempts,
            has_bundle_failures,
            latest_knowledge_generation,
        ),
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

    // One "rebuilding" bucket keyed on in-flight document count (the
    // canonical `processing + queued` split from the metrics row).
    // See `map_library_facts_from_aggregate` for why this is the
    // canonical signal here — no per-doc knowledge revision reads, no
    // drift vs the dashboard counts.
    let in_flight = coverage.metrics.processing + coverage.metrics.queued;
    if in_flight > 0 || coverage.vector_inventory_mismatch_count > 0 {
        warnings.push(derived_warning(library_id, "stale_vectors", "warning", Utc::now()));
    }
    if in_flight > 0 {
        warnings.push(derived_warning(library_id, "stale_relations", "warning", Utc::now()));
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

fn map_async_operation_row(
    row: ops_repository::OpsAsyncOperationRow,
) -> Result<OpsAsyncOperation, ApiError> {
    let status = OpsAsyncOperationStatus::from_db(&row.status)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
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
        created_at: row.created_at,
        completed_at: row.completed_at,
    })
}

fn derive_degraded_state(
    queue_depth: i64,
    running_attempts: i64,
    failed_document_count: usize,
    stale_vector_count: usize,
    stale_relation_count: usize,
    has_failed_attempts: bool,
    has_bundle_failures: bool,
    latest_generation: Option<&KnowledgeLibraryGeneration>,
) -> String {
    if failed_document_count > 0 || has_failed_attempts || has_bundle_failures {
        "degraded".to_string()
    } else if stale_vector_count > 0 || stale_relation_count > 0 {
        "rebuilding".to_string()
    } else if queue_depth > 0 || running_attempts > 0 {
        "processing".to_string()
    } else {
        let _ = latest_generation;
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
        DocumentKnowledgeSignals, LibraryCoverageFast, build_library_warnings_from_aggregate,
        classify_document_knowledge_signals, derive_degraded_state,
        map_library_facts_from_aggregate,
    };
    use crate::domains::knowledge::KnowledgeLibraryGeneration;
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
                graph_ready: 0,
                graph_sparse: 0,
                recomputed_at: Utc::now(),
            },
            graph_snapshot: None,
            latest_generation_id: None,
            vector_inventory_mismatch_count,
        }
    }

    fn sample_generation(state: &str) -> KnowledgeLibraryGeneration {
        KnowledgeLibraryGeneration {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            generation_kind: "library".to_string(),
            generation_state: state.to_string(),
            source_revision_id: None,
            created_at: Utc::now(),
            completed_at: None,
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
                graph_ready: 0,
                graph_sparse: 0,
                recomputed_at: Utc::now(),
            },
            graph_snapshot: None,
            latest_generation_id: None,
            vector_inventory_mismatch_count: 0,
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
        assert_eq!(state.graph_coverage_kind, "processing");
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
        let degraded_state = derive_degraded_state(
            0,
            0,
            0,
            0,
            0,
            false,
            false,
            Some(&sample_generation("graph_ready")),
        );

        assert_eq!(degraded_state, "healthy");
    }

    #[test]
    fn build_library_warnings_ignores_idle_library() {
        let coverage = coverage_with_processing(0);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn build_library_warnings_reports_in_flight_rebuilds() {
        let coverage = coverage_with_processing(3);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);
        assert!(
            warnings
                .iter()
                .any(|warning: &OpsLibraryWarning| warning.warning_kind == "stale_relations")
        );
        assert!(
            warnings
                .iter()
                .any(|warning: &OpsLibraryWarning| warning.warning_kind == "stale_vectors")
        );
    }

    #[test]
    fn build_library_warnings_reports_vector_inventory_mismatch_without_relation_warning() {
        let coverage = coverage_with_processing_and_vector_mismatch(0, 2);
        let warnings = build_library_warnings_from_aggregate(Uuid::now_v7(), &coverage, &[], &[]);

        assert!(
            warnings
                .iter()
                .any(|warning: &OpsLibraryWarning| warning.warning_kind == "stale_vectors")
        );
        assert!(
            warnings
                .iter()
                .all(|warning: &OpsLibraryWarning| warning.warning_kind != "stale_relations")
        );
    }
}
