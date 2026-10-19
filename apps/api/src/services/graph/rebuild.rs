mod revision_merge;
mod revision_projection;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use revision_merge::{
    PureAppendDelta, RevisionGraphMergeSummary, RevisionMergeRecordsInput,
    classify_pure_append_chunks, collect_revision_graph_targets_with_connection,
    merge_revision_graph_records_with_connection, select_latest_revision_extraction_records,
};
use revision_projection::{
    AuthorizedRevisionProjectionInput, RevisionReconcileProjectionPlan,
    authorized_preserve_or_empty_snapshot, execute_authorized_revision_projection,
    is_graph_reconcile_chunk_text_eligible, revision_reconcile_projection_plan, union_targeted_ids,
};

use crate::{
    app::state::AppState,
    domains::ops::{ASYNC_OP_STATUS_READY, GRAPH_STATUS_EMPTY, GRAPH_STATUS_READY},
    infra::knowledge_plane::IngestRevisionWriteAuthority,
    infra::repositories::{
        self, ChunkRow, DocumentRow, RuntimeGraphSnapshotRow, content_chunk_repository,
        content_document_head_repository, content_document_repository, content_revision_repository,
    },
    services::{
        graph::backfill::{
            GraphReextractCandidate, GraphReextractOutcome, REEXTRACT_MAX_BATCH_SIZE,
            enqueue_graph_reextract_jobs,
        },
        graph::error::GraphServiceError,
        graph::extract::{
            GRAPH_EXTRACTION_VERSION, GraphExtractionCandidateSet,
            extraction_lifecycle_from_record, extraction_recovery_summary_from_record,
            repair_graph_extraction_candidate_set, repair_graph_extraction_normalized_json,
        },
        graph::merge::{
            GraphMergeOutcome, GraphMergeScope, MergeChunkInput,
            merge_chunk_graph_candidates_with_connection,
        },
        graph::projection::{
            GraphProjectionOutcome, GraphProjectionScope, next_projection_version,
            project_rebuild_normalized_graph_with_connection, schedule_topology_prewarm,
        },
        graph::summary::PendingGraphSummaryRefresh,
        ingest::cancellation::ensure_not_cancelled,
    },
    shared::extraction::text_quality::is_graph_extraction_text_eligible,
};

const MIN_REBUILD_PUBLICATION_FLOOR_PERCENT: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyGraphDocument {
    workspace_id: Uuid,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
    graph_contribution_count: Option<usize>,
}

#[derive(Debug)]
enum RebuildTransactionOutcome {
    Published(GraphProjectionOutcome),
    CoverageRejected(RebuildCoverageFailure),
}

pub(crate) async fn rebuild_library_graph(
    state: &AppState,
    library_id: Uuid,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let mut guard = match repositories::acquire_runtime_library_graph_rebuild_guard(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to acquire atomic runtime graph rebuild guard")?
    {
        repositories::RuntimeGraphRebuildGuardAcquireOutcome::Acquired(guard) => guard,
        repositories::RuntimeGraphRebuildGuardAcquireOutcome::LibraryNotFound => {
            return Err(GraphServiceError::LibraryNotFound { library_id });
        }
        repositories::RuntimeGraphRebuildGuardAcquireOutcome::Busy => {
            return Err(GraphServiceError::WriteContention {
                message: "another whole-library graph rebuild is already running".to_string(),
            });
        }
    };

    let transaction_result =
        rebuild_library_graph_in_transaction(state, library_id, guard.connection()).await;
    match transaction_result {
        Ok(RebuildTransactionOutcome::Published(outcome)) => {
            guard.commit().await.context("failed to commit atomic runtime graph rebuild")?;
            schedule_topology_prewarm(state, library_id);
            Ok(outcome)
        }
        Ok(RebuildTransactionOutcome::CoverageRejected(failure)) => {
            guard.rollback().await.context("failed to roll back rejected runtime graph rebuild")?;
            let recovery =
                enqueue_graph_reextract_jobs(state, library_id, &failure.recovery_candidates).await;
            Err(failure.into_graph_error(library_id, &recovery))
        }
        Err(error) => {
            let rollback_result =
                guard.rollback().await.context("failed to roll back atomic runtime graph rebuild");
            match rollback_result {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(rollback_error.context(error.to_string()).into()),
            }
        }
    }
}

async fn rebuild_library_graph_in_transaction(
    state: &AppState,
    library_id: Uuid,
    connection: &mut sqlx::PgConnection,
) -> Result<RebuildTransactionOutcome, GraphServiceError> {
    let snapshot =
        repositories::get_runtime_graph_snapshot_with_executor(&mut *connection, library_id)
            .await
            .context("failed to load graph snapshot while planning rebuild")?;
    let projection_version = next_projection_version(snapshot.as_ref());
    let ready_documents =
        load_ready_graph_documents_with_connection(connection, library_id).await?;
    let baseline_document_ids = load_rebuild_baseline_document_ids_with_connection(
        connection,
        library_id,
        snapshot.as_ref(),
        &ready_documents,
    )
    .await?;
    let extractions = repositories::runtime_graph_repository::
        list_runtime_graph_extraction_records_by_library_with_executor(
            &mut *connection,
            library_id,
            GRAPH_EXTRACTION_VERSION,
        )
        .await
        .context("failed to reload runtime graph extraction records for rebuild")?;

    if extractions.is_empty() {
        let coverage = match assess_rebuild_coverage(
            projection_version,
            &ready_documents,
            &baseline_document_ids,
            &BTreeSet::new(),
        ) {
            Ok(coverage) => coverage,
            Err(failure) => return Ok(RebuildTransactionOutcome::CoverageRejected(failure)),
        };
        if let Some(error) = empty_rebuild_conflict(
            library_id,
            snapshot.as_ref(),
            &ready_documents,
            coverage,
            "no graph extraction records matched the current extraction version",
        ) {
            return Err(error);
        }
        let projection = project_rebuild_normalized_graph_with_connection(
            state,
            connection,
            &GraphProjectionScope::new(library_id, projection_version),
        )
        .await?;
        return Ok(RebuildTransactionOutcome::Published(projection));
    }

    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();

    for record in extractions {
        let expected_completion = ready_documents.get(&record.document_id);
        let Some(merge_outcome) = rebuild_graph_extraction_record_with_connection(
            state,
            connection,
            library_id,
            projection_version,
            expected_completion,
            record,
        )
        .await?
        else {
            continue;
        };
        changed_node_ids.extend(merge_outcome.summary_refresh_node_ids());
        changed_edge_ids.extend(merge_outcome.summary_refresh_edge_ids());
    }

    repositories::recalculate_runtime_graph_support_counts_by_ids_with_executor(
        connection,
        library_id,
        projection_version,
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to reconcile rebuilt graph support counts")?;

    let merged_nodes = repositories::list_admitted_runtime_graph_nodes_by_library_with_executor(
        &mut *connection,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph nodes")?;
    let projected_document_ids = runtime_graph_document_ids(&merged_nodes);

    let coverage = match assess_rebuild_coverage(
        projection_version,
        &ready_documents,
        &baseline_document_ids,
        &projected_document_ids,
    ) {
        Ok(coverage) => coverage,
        Err(failure) => return Ok(RebuildTransactionOutcome::CoverageRejected(failure)),
    };

    let merged_edges = repositories::list_admitted_runtime_graph_edges_by_library_with_executor(
        &mut *connection,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph edges")?;

    if merged_nodes.is_empty() && merged_edges.is_empty() {
        if let Some(error) = empty_rebuild_conflict(
            library_id,
            snapshot.as_ref(),
            &ready_documents,
            coverage,
            "the rebuilt graph produced no admitted nodes or edges",
        ) {
            return Err(error);
        }
        let projection = project_rebuild_normalized_graph_with_connection(
            state,
            connection,
            &GraphProjectionScope::new(library_id, projection_version),
        )
        .await?;
        return Ok(RebuildTransactionOutcome::Published(projection));
    }

    let projection_scope = GraphProjectionScope::new(library_id, projection_version);
    let projection =
        project_rebuild_normalized_graph_with_connection(state, connection, &projection_scope)
            .await
            .context("failed to project rebuilt graph")?;
    Ok(RebuildTransactionOutcome::Published(projection))
}

async fn rebuild_graph_extraction_record_with_connection(
    state: &AppState,
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    expected_completion: Option<&ReadyGraphDocument>,
    record: repositories::RuntimeGraphExtractionRecordRow,
) -> Result<Option<GraphMergeOutcome>, GraphServiceError> {
    if record.status != ASYNC_OP_STATUS_READY {
        return Ok(None);
    }
    let Some(document_row) = content_document_repository::get_document_by_id_with_executor(
        &mut *connection,
        record.document_id,
    )
    .await
    .with_context(|| format!("failed to load document {}", record.document_id))?
    else {
        return Ok(None);
    };
    if record.library_id != library_id
        || document_row.library_id != library_id
        || document_row.deleted_at.is_some()
    {
        return Ok(None);
    }
    let Some(document_head) = content_document_head_repository::get_document_head_with_executor(
        &mut *connection,
        record.document_id,
    )
    .await
    .with_context(|| format!("failed to load document head {}", record.document_id))?
    else {
        return Ok(None);
    };
    let extraction_lifecycle = extraction_lifecycle_from_record(&record);
    let Some(revision_id) = extraction_lifecycle.revision_id else {
        return Ok(None);
    };
    let Some(expected_completion) = expected_completion else {
        return Ok(None);
    };
    if !expected_completion.graph_contribution_count.is_some_and(|count| count > 0)
        || expected_completion.revision_id != revision_id
        || expected_completion.activated_by_attempt_id
            != extraction_lifecycle.activated_by_attempt_id
        || active_readable_revision_id(&document_head) != Some(revision_id)
    {
        return Ok(None);
    }
    let Some(revision) = content_revision_repository::get_revision_by_id_with_executor(
        &mut *connection,
        revision_id,
    )
    .await
    .with_context(|| format!("failed to load revision {revision_id}"))?
    else {
        return Ok(None);
    };
    if revision.document_id != record.document_id
        || revision.workspace_id != document_row.workspace_id
        || revision.library_id != library_id
    {
        return Ok(None);
    }
    let Some(chunk_row) =
        content_chunk_repository::get_chunk_by_id_with_executor(&mut *connection, record.chunk_id)
            .await
            .with_context(|| format!("failed to load chunk {}", record.chunk_id))?
    else {
        return Ok(None);
    };
    if chunk_row.revision_id != revision_id
        || !is_graph_reconcile_chunk_text_eligible(&chunk_row.normalized_text)
    {
        return Ok(None);
    }

    let document = synthesize_document_row(&document_row, &document_head, Some(&revision));
    let chunk = synthesize_chunk_row(&chunk_row, document_row.id, library_id, revision.created_at);
    let candidates = repaired_graph_extraction_candidates(record.normalized_output_json.clone())
        .with_context(|| {
            format!(
                "failed to decode normalized graph extraction for document {} chunk {}",
                record.document_id, record.chunk_id
            )
        })?;
    if candidates.entities.is_empty() && candidates.relations.is_empty() {
        return Ok(None);
    }

    let merge_scope = GraphMergeScope::new(library_id, projection_version)
        .with_lifecycle(Some(revision_id), extraction_lifecycle.activated_by_attempt_id);
    let extraction_recovery = extraction_recovery_summary_from_record(&record);
    merge_chunk_graph_candidates_with_connection(
        connection,
        &state.bulk_ingest_hardening_services.graph_quality_guard,
        &merge_scope,
        MergeChunkInput {
            document: &document,
            chunk: &chunk,
            candidates: &candidates,
            extraction_recovery: extraction_recovery.as_ref(),
        },
    )
    .await
    .with_context(|| {
        format!("failed to rebuild graph knowledge for document {} chunk {}", document.id, chunk.id)
    })
    .map(Some)
    .map_err(Into::into)
}

#[derive(Debug, Clone)]
pub struct RevisionGraphReconcileOutcome {
    pub projection: GraphProjectionOutcome,
    pub graph_contribution_count: usize,
    pub graph_ready: bool,
    pub pending_summary_refresh: Option<PendingGraphSummaryRefresh>,
}

pub(crate) async fn reconcile_revision_graph_authorized(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
    connection: &mut sqlx::PgConnection,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphReconcileOutcome, GraphServiceError> {
    ensure_not_cancelled(cancellation_token)?;
    let document_row = content_document_repository::get_document_by_id_with_executor(
        &mut *connection,
        document_id,
    )
    .await
    .with_context(|| format!("failed to load content document {document_id}"))?
    .with_context(|| format!("content document {document_id} not found"))?;
    let document_head =
        content_document_head_repository::get_document_head_for_update_with_executor(
            &mut *connection,
            document_id,
        )
        .await
        .with_context(|| format!("failed to load content document head {document_id}"))?
        .with_context(|| format!("content document head {document_id} not found"))?;
    let revision = content_revision_repository::get_revision_by_id_with_executor(
        &mut *connection,
        authority.revision_id,
    )
    .await
    .with_context(|| format!("failed to load content revision {}", authority.revision_id))?
    .with_context(|| format!("content revision {} not found", authority.revision_id))?;
    let revision_chunks = content_chunk_repository::list_chunks_by_revision_with_executor(
        &mut *connection,
        authority.revision_id,
    )
    .await
    .with_context(|| {
        format!("failed to list chunks for content revision {}", authority.revision_id)
    })?;
    if document_row.workspace_id != authority.workspace_id
        || document_row.library_id != authority.library_id
        || revision.workspace_id != authority.workspace_id
        || revision.library_id != authority.library_id
        || revision.document_id != document_id
    {
        return Err(GraphServiceError::StateConflict {
            message: "authorized graph reconcile scope mismatch".to_string(),
        });
    }
    ensure_not_cancelled(cancellation_token)?;

    let document = synthesize_document_row(&document_row, &document_head, Some(&revision));
    let revision_chunk_ids = revision_chunks.iter().map(|chunk| chunk.id).collect::<BTreeSet<_>>();
    let chunk_rows_by_id = revision_chunks
        .iter()
        .map(|chunk| {
            (
                chunk.id,
                synthesize_chunk_row(chunk, document_id, authority.library_id, revision.created_at),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let previous_active_revision_id = document_head
        .active_revision_id
        .filter(|revision_id| *revision_id != authority.revision_id);

    let snapshot = repositories::get_runtime_graph_snapshot_with_executor(
        &mut *connection,
        authority.library_id,
    )
    .await
    .context("failed to load graph snapshot while reconciling authorized revision graph")?;
    let projection_scope = GraphProjectionScope::new(
        authority.library_id,
        crate::services::graph::projection::active_projection_version(snapshot.as_ref()),
    )
    .defer_source_truth_to_lifecycle();
    let existing_graph_is_empty =
        snapshot.as_ref().is_none_or(|row| row.node_count <= 0 && row.edge_count <= 0);

    // Deletion convergence owns removal of a deleted document's evidence.
    // Bail out before retry replacement mutates any evidence: preserving an
    // old snapshot after deleting its support rows would publish internally
    // inconsistent counts. The document/head locks held above serialize this
    // decision with deletion lifecycle publication.
    if document_row.document_state == "deleted" || document_row.deleted_at.is_some() {
        return preserve_deleted_document_graph_snapshot(
            connection,
            &projection_scope,
            snapshot.as_ref(),
        )
        .await;
    }

    // Same-revision retry replacement is mandatory: remove all evidence from
    // A before merging B. Previous-revision evidence is removed as a separate
    // footprint when the head is moving to a new revision.
    let (mut replaced_node_ids, mut replaced_edge_ids) =
        collect_revision_graph_targets_with_connection(
            connection,
            authority.library_id,
            document_id,
            authority.revision_id,
            false,
            cancellation_token,
        )
        .await?;
    // A pure append carries every previous chunk forward at the same index with
    // the same text checksum. Its evidence is moved to the new revision and the
    // new chunk ids in place; only the appended chunks are extracted and merged
    // again. Any other change (edit, delete, reorder) keeps the full teardown
    // path below unchanged.
    let pure_append = match previous_active_revision_id {
        Some(previous_revision_id) => {
            let previous_chunks = content_chunk_repository::list_chunks_by_revision_with_executor(
                &mut *connection,
                previous_revision_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to list previous revision chunks {previous_revision_id} while classifying append"
                )
            })?;
            classify_pure_append_chunks(&previous_chunks, &revision_chunks)
        }
        None => None,
    };
    let carried_evidence_target_count = carry_previous_revision_graph_evidence(
        connection,
        PreviousRevisionCarry {
            authority: &authority,
            document_id,
            previous_active_revision_id,
            pure_append: pure_append.as_ref(),
            cancellation_token,
        },
        &mut replaced_node_ids,
        &mut replaced_edge_ids,
    )
    .await?;

    let extraction_records =
        repositories::list_runtime_graph_extraction_records_by_document_with_executor(
            &mut *connection,
            document_id,
            GRAPH_EXTRACTION_VERSION,
        )
        .await
        .with_context(|| {
            format!("failed to list graph extraction records for document {document_id}")
        })?;
    let mut latest_records_by_chunk = select_latest_revision_extraction_records(
        extraction_records,
        &revision_chunk_ids,
        authority.revision_id,
        authority.attempt_id,
        cancellation_token,
    )?;
    if let Some(delta) = pure_append.as_ref() {
        latest_records_by_chunk.retain(|chunk_id, _| delta.added_chunk_ids.contains(chunk_id));
    }
    let merge_scope =
        GraphMergeScope::new(authority.library_id, projection_scope.projection_version)
            .with_lifecycle(Some(authority.revision_id), Some(authority.attempt_id));
    let merge_summary = merge_revision_graph_records_with_connection(
        connection,
        state,
        RevisionMergeRecordsInput {
            document,
            chunk_rows_by_id,
            merge_scope,
            records: latest_records_by_chunk,
        },
        cancellation_token,
    )
    .await?;
    let graph_contribution_count =
        merge_summary.graph_contribution_count.saturating_add(carried_evidence_target_count);
    let targeted_node_ids = union_targeted_ids(&merge_summary.changed_node_ids, &replaced_node_ids);
    let targeted_edge_ids = union_targeted_ids(&merge_summary.changed_edge_ids, &replaced_edge_ids);
    let plan = revision_reconcile_projection_plan(
        previous_active_revision_id.is_some()
            || !replaced_node_ids.is_empty()
            || !replaced_edge_ids.is_empty(),
        graph_contribution_count,
        existing_graph_is_empty,
        snapshot.is_some(),
        &targeted_node_ids,
        &targeted_edge_ids,
    );
    let pending_summary_refresh = pending_summary_refresh_for_plan(plan, &merge_summary);
    let projection = execute_authorized_revision_projection(
        connection,
        &projection_scope,
        AuthorizedRevisionProjectionInput {
            authority,
            document_id,
            snapshot: snapshot.as_ref(),
            plan,
            targeted_node_ids: &targeted_node_ids,
            targeted_edge_ids: &targeted_edge_ids,
        },
        cancellation_token,
    )
    .await?;
    Ok(RevisionGraphReconcileOutcome {
        graph_ready: graph_contribution_count > 0 && projection.graph_status == GRAPH_STATUS_READY,
        graph_contribution_count,
        pending_summary_refresh: Some(pending_summary_refresh),
        projection,
    })
}

/// Inputs for carrying or replacing a previous revision's graph evidence.
struct PreviousRevisionCarry<'a> {
    authority: &'a IngestRevisionWriteAuthority,
    document_id: Uuid,
    previous_active_revision_id: Option<Uuid>,
    pure_append: Option<&'a PureAppendDelta>,
    cancellation_token: &'a CancellationToken,
}

/// Carries a pure append's evidence forward or replaces the previous revision's
/// footprint, extending the replaced id sets. Returns the carried target count.
async fn carry_previous_revision_graph_evidence(
    connection: &mut sqlx::PgConnection,
    carry: PreviousRevisionCarry<'_>,
    replaced_node_ids: &mut BTreeSet<Uuid>,
    replaced_edge_ids: &mut BTreeSet<Uuid>,
) -> Result<usize, GraphServiceError> {
    let PreviousRevisionCarry {
        authority,
        document_id,
        previous_active_revision_id,
        pure_append,
        cancellation_token,
    } = carry;
    let mut carried_evidence_target_count = 0usize;
    if let Some(previous_revision_id) = previous_active_revision_id {
        if let Some(delta) = pure_append {
            repositories::query_repository::delete_query_execution_references_by_content_revision_with_executor(
                &mut *connection,
                authority.library_id,
                document_id,
                previous_revision_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to delete superseded query references for document {document_id} revision {previous_revision_id}"
                )
            })?;
            let carried = repositories::runtime_graph_repository::retag_runtime_graph_evidence_to_revision(
                &mut *connection,
                repositories::runtime_graph_repository::RetagRuntimeGraphEvidenceInput {
                    library_id: authority.library_id,
                    document_id,
                    previous_revision_id,
                    revision_id: authority.revision_id,
                    activated_by_attempt_id: authority.attempt_id,
                    kept_previous_chunk_ids: &delta.kept_previous_chunk_ids(),
                    kept_revision_chunk_ids: &delta.kept_revision_chunk_ids(),
                },
            )
            .await
            .with_context(|| {
                format!(
                    "failed to retag carried graph evidence for document {document_id} revision {previous_revision_id}"
                )
            })?;
            carried_evidence_target_count = carried.len();
        } else {
            let (nodes, edges) = collect_revision_graph_targets_with_connection(
                connection,
                authority.library_id,
                document_id,
                previous_revision_id,
                true,
                cancellation_token,
            )
            .await?;
            replaced_node_ids.extend(nodes);
            replaced_edge_ids.extend(edges);
        }
    }
    Ok(carried_evidence_target_count)
}

/// Preserves or empties a deleted document's snapshot without mutating evidence.
async fn preserve_deleted_document_graph_snapshot(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    snapshot: Option<&RuntimeGraphSnapshotRow>,
) -> Result<RevisionGraphReconcileOutcome, GraphServiceError> {
    let projection = authorized_preserve_or_empty_snapshot(connection, scope, snapshot).await?;
    Ok(RevisionGraphReconcileOutcome {
        graph_ready: false,
        graph_contribution_count: 0,
        pending_summary_refresh: None,
        projection,
    })
}

/// Selects the queued summary refresh for a reconcile projection plan.
fn pending_summary_refresh_for_plan(
    plan: RevisionReconcileProjectionPlan,
    merge_summary: &RevisionGraphMergeSummary,
) -> PendingGraphSummaryRefresh {
    if plan.broad_summary_refresh() {
        PendingGraphSummaryRefresh::broad()
    } else {
        PendingGraphSummaryRefresh::targeted(
            merge_summary.changed_node_ids.clone(),
            merge_summary.changed_edge_ids.clone(),
        )
    }
}

fn empty_rebuild_conflict(
    library_id: Uuid,
    snapshot: Option<&RuntimeGraphSnapshotRow>,
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
    coverage: RebuildCoverageCounts,
    reason: &str,
) -> Option<GraphServiceError> {
    let snapshot = snapshot
        .filter(|snapshot| snapshot.graph_status == GRAPH_STATUS_READY)
        .filter(|snapshot| snapshot.node_count > 0 || snapshot.edge_count > 0)?;
    if authoritative_zero_rebuild_is_proven(ready_documents, coverage) {
        return None;
    }
    Some(GraphServiceError::StateConflict {
        message: format!(
            "runtime graph rebuild for library {library_id} would publish an empty projection because {reason}, but active projection {} is ready with {} nodes and {} edges; rebuild source material is inconsistent and the active projection was left unchanged",
            snapshot.projection_version, snapshot.node_count, snapshot.edge_count
        ),
    })
}

fn authoritative_zero_rebuild_is_proven(
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
    coverage: RebuildCoverageCounts,
) -> bool {
    // The completion query left-joins every readable current head, so an
    // all-zero, exact-attempt result proves the absence of graph input. Keep
    // the set non-empty so a vacuous no-readable-document result cannot retire
    // a live projection without affirmative lifecycle evidence.
    coverage.is_publishable()
        && !ready_documents.is_empty()
        && ready_documents.values().all(|document| {
            document.activated_by_attempt_id.is_some()
                && document.graph_contribution_count == Some(0)
        })
}

async fn load_ready_graph_documents_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
) -> Result<BTreeMap<Uuid, ReadyGraphDocument>, GraphServiceError> {
    let completion_rows = repositories::runtime_graph_repository::
        list_graph_rebuild_document_completions_with_executor(
            &mut *connection,
            library_id,
            GRAPH_EXTRACTION_VERSION,
        )
        .await
        .context("failed to load graph extraction completions for rebuild coverage")?;
    Ok(completion_rows
        .into_iter()
        .map(|completion| {
            (
                completion.document_id,
                ReadyGraphDocument {
                    workspace_id: completion.workspace_id,
                    revision_id: completion.revision_id,
                    activated_by_attempt_id: completion.activated_by_attempt_id,
                    graph_contribution_count: completion.graph_contribution_count,
                },
            )
        })
        .collect())
}

fn active_readable_revision_id(
    head: &content_document_head_repository::ContentDocumentHeadRow,
) -> Option<Uuid> {
    let active_revision_id = head.active_revision_id?;
    (head.readable_revision_id == Some(active_revision_id)).then_some(active_revision_id)
}

async fn load_rebuild_baseline_document_ids_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    snapshot: Option<&RuntimeGraphSnapshotRow>,
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
) -> Result<BTreeSet<Uuid>, GraphServiceError> {
    let Some(snapshot) = snapshot.filter(|snapshot| snapshot.graph_status == GRAPH_STATUS_READY)
    else {
        return Ok(BTreeSet::new());
    };
    let active_document_ids = load_projection_document_ids_with_connection(
        connection,
        library_id,
        snapshot.projection_version,
    )
    .await?;
    let previous_document_ids = if snapshot.projection_version > 1 {
        load_projection_document_ids_with_connection(
            connection,
            library_id,
            snapshot.projection_version - 1,
        )
        .await?
    } else {
        BTreeSet::new()
    };
    Ok(select_rebuild_baseline_document_ids(
        active_document_ids,
        previous_document_ids,
        ready_documents,
    ))
}

async fn load_projection_document_ids_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
) -> Result<BTreeSet<Uuid>, GraphServiceError> {
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_library_with_executor(
        &mut *connection,
        library_id,
        projection_version,
    )
    .await
    .with_context(|| {
        format!(
            "failed to load projection {projection_version} document baseline for graph rebuild"
        )
    })?;
    Ok(runtime_graph_document_ids(&nodes))
}

fn runtime_graph_document_ids(nodes: &[repositories::RuntimeGraphNodeRow]) -> BTreeSet<Uuid> {
    nodes
        .iter()
        .filter(|node| node.node_type == "document")
        .filter_map(|node| node.identity_key.strip_prefix("document:"))
        .filter_map(|document_id| Uuid::parse_str(document_id).ok())
        .collect()
}

fn select_rebuild_baseline_document_ids(
    mut active_document_ids: BTreeSet<Uuid>,
    mut previous_document_ids: BTreeSet<Uuid>,
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
) -> BTreeSet<Uuid> {
    let requires_retention = |document_id: &Uuid| {
        ready_documents
            .get(document_id)
            .is_some_and(|document| document.graph_contribution_count != Some(0))
    };
    active_document_ids.retain(|document_id| requires_retention(document_id));
    previous_document_ids.retain(requires_retention);
    if previous_document_ids.len() > active_document_ids.len() {
        previous_document_ids
    } else {
        active_document_ids
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RebuildCoverageCounts {
    retained_baseline: usize,
    baseline: usize,
    completed: usize,
    ready: usize,
    projected_positive: usize,
    positive: usize,
}

#[derive(Debug)]
struct RebuildCoverageFailure {
    projection_version: i64,
    coverage: RebuildCoverageCounts,
    recovery_candidates: Vec<GraphReextractCandidate>,
}

impl RebuildCoverageFailure {
    fn into_graph_error(
        self,
        library_id: Uuid,
        recovery: &GraphReextractOutcome,
    ) -> GraphServiceError {
        let coverage = self.coverage;
        let retention_percent = coverage_percent(coverage.retained_baseline, coverage.baseline);
        let completion_coverage_percent = coverage_percent(coverage.completed, coverage.ready);
        let positive_projection_percent =
            coverage_percent(coverage.projected_positive, coverage.positive);
        GraphServiceError::StateConflict {
            message: format!(
                "runtime graph rebuild for library {library_id} refused projection {}: it retains {} of {} baseline documents ({retention_percent:.2}%), has current completion markers for {} of {} ready documents ({completion_coverage_percent:.2}%), and projects {} of {} positive-contribution completions ({positive_projection_percent:.2}%); all three must meet the {MIN_REBUILD_PUBLICATION_FLOOR_PERCENT}% publication floor; active projection was left unchanged, all staging writes were rolled back, and graph_refresh recovery jobs were enqueued={}, deduplicated={}, failed={}",
                self.projection_version,
                coverage.retained_baseline,
                coverage.baseline,
                coverage.completed,
                coverage.ready,
                coverage.projected_positive,
                coverage.positive,
                recovery.enqueued,
                recovery.skipped_dedupe,
                recovery.failures,
            ),
        }
    }
}

impl RebuildCoverageCounts {
    fn is_publishable(self) -> bool {
        rebuild_coverage_meets_publication_floor(self.retained_baseline, self.baseline)
            && rebuild_coverage_meets_publication_floor(self.completed, self.ready)
            && rebuild_coverage_meets_publication_floor(self.projected_positive, self.positive)
    }
}

fn rebuild_coverage_counts(
    projected_document_ids: &BTreeSet<Uuid>,
    baseline_document_ids: &BTreeSet<Uuid>,
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
) -> RebuildCoverageCounts {
    let retained_baseline = projected_document_ids.intersection(baseline_document_ids).count();
    let completed = ready_documents
        .values()
        .filter(|document| document.graph_contribution_count.is_some())
        .count();
    let positive = ready_documents
        .values()
        .filter(|document| document.graph_contribution_count.is_some_and(|count| count > 0))
        .count();
    let projected_positive = ready_documents
        .iter()
        .filter(|(document_id, document)| {
            document.graph_contribution_count.is_some_and(|count| count > 0)
                && projected_document_ids.contains(document_id)
        })
        .count();
    RebuildCoverageCounts {
        retained_baseline,
        baseline: baseline_document_ids.len(),
        completed,
        ready: ready_documents.len(),
        projected_positive,
        positive,
    }
}

fn rebuild_coverage_meets_publication_floor(
    covered_document_count: usize,
    required_document_count: usize,
) -> bool {
    if required_document_count == 0 {
        return true;
    }
    (covered_document_count as u128) * 100
        >= (required_document_count as u128) * (MIN_REBUILD_PUBLICATION_FLOOR_PERCENT as u128)
}

fn missing_reextract_candidates(
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
    projected_document_ids: &BTreeSet<Uuid>,
) -> Vec<GraphReextractCandidate> {
    ready_documents
        .iter()
        .filter(|(document_id, document)| match document.graph_contribution_count {
            None => true,
            Some(0) => false,
            Some(_) => !projected_document_ids.contains(document_id),
        })
        .map(|(document_id, document)| GraphReextractCandidate {
            workspace_id: document.workspace_id,
            document_id: *document_id,
            revision_id: document.revision_id,
        })
        .take(usize::try_from(REEXTRACT_MAX_BATCH_SIZE).unwrap_or(usize::MAX))
        .collect()
}

fn assess_rebuild_coverage(
    projection_version: i64,
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
    baseline_document_ids: &BTreeSet<Uuid>,
    projected_document_ids: &BTreeSet<Uuid>,
) -> Result<RebuildCoverageCounts, RebuildCoverageFailure> {
    let coverage =
        rebuild_coverage_counts(projected_document_ids, baseline_document_ids, ready_documents);
    if coverage.is_publishable() {
        return Ok(coverage);
    }

    let recovery_candidates = missing_reextract_candidates(ready_documents, projected_document_ids);
    Err(RebuildCoverageFailure { projection_version, coverage, recovery_candidates })
}

fn coverage_percent(covered: usize, total: usize) -> f64 {
    if total == 0 { 100.0 } else { covered as f64 * 100.0 / total as f64 }
}

fn repaired_graph_extraction_candidates(
    normalized_output_json: serde_json::Value,
) -> anyhow::Result<GraphExtractionCandidateSet> {
    serde_json::from_value::<GraphExtractionCandidateSet>(repair_graph_extraction_normalized_json(
        normalized_output_json,
    ))
    .map(repair_graph_extraction_candidate_set)
    .context("normalized graph extraction does not match the candidate contract")
}

fn synthesize_document_row(
    document_row: &content_document_repository::ContentDocumentRow,
    document_head: &content_document_head_repository::ContentDocumentHeadRow,
    revision: Option<&content_revision_repository::ContentRevisionRow>,
) -> DocumentRow {
    DocumentRow {
        id: document_row.id,
        library_id: document_row.library_id,
        source_id: None,
        external_key: document_row.external_key.clone(),
        title: revision.and_then(|value| value.title.clone()),
        mime_type: revision.map(|value| value.mime_type.clone()),
        checksum: revision.map(|value| value.checksum.clone()),
        active_revision_id: document_head.active_revision_id,
        document_state: document_row.document_state.clone(),
        mutation_kind: None,
        mutation_status: None,
        deleted_at: document_row.deleted_at,
        created_at: document_row.created_at,
        updated_at: document_head.head_updated_at,
    }
}

fn synthesize_chunk_row(
    chunk_row: &content_chunk_repository::ContentChunkRow,
    document_id: Uuid,
    library_id: Uuid,
    created_at: chrono::DateTime<Utc>,
) -> ChunkRow {
    ChunkRow {
        id: chunk_row.id,
        document_id,
        library_id,
        ordinal: chunk_row.chunk_index,
        content: chunk_row.normalized_text.clone(),
        token_count: chunk_row.token_count,
        metadata_json: serde_json::json!({
            "revision_id": chunk_row.revision_id,
            "start_offset": chunk_row.start_offset,
            "end_offset": chunk_row.end_offset,
            "text_checksum": chunk_row.text_checksum,
        }),
        created_at,
    }
}
