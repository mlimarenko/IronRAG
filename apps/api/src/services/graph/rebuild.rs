use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use futures::stream::{self, StreamExt, TryStreamExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{ASYNC_OP_STATUS_READY, GRAPH_STATUS_READY},
    infra::repositories::{
        self, ChunkRow, DocumentRow, RuntimeGraphSnapshotRow, content_repository,
    },
    services::{
        graph::error::GraphServiceError,
        graph::extract::{
            GRAPH_EXTRACTION_VERSION, GraphExtractionCandidateSet,
            extraction_lifecycle_from_record, extraction_recovery_summary_from_record,
            repair_graph_extraction_candidate_set, repair_graph_extraction_normalized_json,
        },
        graph::merge::{
            GraphMergeOutcome, GraphMergeScope, merge_chunk_graph_candidates,
            reconcile_merge_support_counts,
        },
        graph::projection::{
            GraphProjectionOutcome, GraphProjectionScope, ensure_empty_graph_snapshot,
            next_projection_version, persist_runtime_graph_snapshot, project_canonical_graph,
        },
        graph::summary::PendingGraphSummaryRefresh,
        ingest::cancellation::{StageError, ensure_not_cancelled},
    },
    shared::extraction::text_quality::is_graph_extraction_text_eligible,
};

pub(crate) async fn rebuild_library_graph(
    state: &AppState,
    library_id: Uuid,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while planning rebuild")?;
    let projection_version = next_projection_version(snapshot.as_ref());
    let extractions = repositories::list_runtime_graph_extraction_records_by_library(
        &state.persistence.postgres,
        library_id,
        GRAPH_EXTRACTION_VERSION,
    )
    .await
    .context("failed to reload runtime graph extraction records for rebuild")?;

    if extractions.is_empty() {
        if let Some(error) = empty_rebuild_conflict(
            library_id,
            snapshot.as_ref(),
            "no graph extraction records matched the current extraction version",
        ) {
            return Err(error);
        }
        return ensure_empty_graph_snapshot(
            state,
            &GraphProjectionScope::new(library_id, projection_version),
        )
        .await;
    }

    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();

    for record in extractions {
        let Some(merge_outcome) =
            rebuild_graph_extraction_record(state, library_id, projection_version, record).await?
        else {
            continue;
        };
        changed_node_ids.extend(merge_outcome.summary_refresh_node_ids());
        changed_edge_ids.extend(merge_outcome.summary_refresh_edge_ids());
    }

    reconcile_merge_support_counts(
        &state.persistence.postgres,
        &GraphMergeScope::new(library_id, projection_version),
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to reconcile rebuilt graph support counts")?;

    let merged_nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph nodes")?;
    let merged_edges = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph edges")?;

    if merged_nodes.is_empty() && merged_edges.is_empty() {
        if let Some(error) = empty_rebuild_conflict(
            library_id,
            snapshot.as_ref(),
            "the rebuilt graph produced no admitted nodes or edges",
        ) {
            return Err(error);
        }
        return ensure_empty_graph_snapshot(
            state,
            &GraphProjectionScope::new(library_id, projection_version),
        )
        .await;
    }

    let projection_scope = GraphProjectionScope::new(library_id, projection_version);
    run_rebuild_projection(state, &projection_scope, "failed to project rebuilt graph")
        .await
        .map_err(Into::into)
}

async fn rebuild_graph_extraction_record(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    record: repositories::RuntimeGraphExtractionRecordRow,
) -> Result<Option<GraphMergeOutcome>, GraphServiceError> {
    if record.status != ASYNC_OP_STATUS_READY {
        return Ok(None);
    }
    let Some(document_row) =
        content_repository::get_document_by_id(&state.persistence.postgres, record.document_id)
            .await
            .with_context(|| format!("failed to load document {}", record.document_id))?
    else {
        return Ok(None);
    };
    if document_row.deleted_at.is_some() {
        return Ok(None);
    }
    let Some(document_head) =
        content_repository::get_document_head(&state.persistence.postgres, record.document_id)
            .await
            .with_context(|| format!("failed to load document head {}", record.document_id))?
    else {
        return Ok(None);
    };
    let extraction_lifecycle = extraction_lifecycle_from_record(&record);
    if extraction_lifecycle.revision_id.is_some()
        && extraction_lifecycle.revision_id != document_head.active_revision_id
    {
        return Ok(None);
    }
    let active_revision_id = extraction_lifecycle.revision_id.or(document_head.active_revision_id);
    let revision = load_optional_content_revision(state, active_revision_id).await?;
    let Some(chunk_row) =
        content_repository::get_chunk_by_id(&state.persistence.postgres, record.chunk_id)
            .await
            .with_context(|| format!("failed to load chunk {}", record.chunk_id))?
    else {
        return Ok(None);
    };
    if !is_graph_reconcile_chunk_text_eligible(&chunk_row.normalized_text) {
        return Ok(None);
    }

    let document = synthesize_document_row(&document_row, &document_head, revision.as_ref());
    let chunk = synthesize_chunk_row(
        &chunk_row,
        document_row.id,
        library_id,
        revision.as_ref().map_or_else(Utc::now, |value| value.created_at),
    );
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
        .with_lifecycle(active_revision_id, extraction_lifecycle.activated_by_attempt_id);
    merge_chunk_graph_candidates(
        &state.persistence.postgres,
        &state.bulk_ingest_hardening_services.graph_quality_guard,
        &merge_scope,
        &document,
        &chunk,
        &candidates,
        extraction_recovery_summary_from_record(&record).as_ref(),
    )
    .await
    .with_context(|| {
        format!("failed to rebuild graph knowledge for document {} chunk {}", document.id, chunk.id)
    })
    .map(Some)
    .map_err(Into::into)
}

async fn load_optional_content_revision(
    state: &AppState,
    revision_id: Option<Uuid>,
) -> Result<Option<content_repository::ContentRevisionRow>, GraphServiceError> {
    let Some(revision_id) = revision_id else {
        return Ok(None);
    };
    content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
        .await
        .with_context(|| format!("failed to load revision {revision_id}"))
        .map_err(Into::into)
}

#[derive(Debug, Clone)]
pub struct RevisionGraphReconcileOutcome {
    pub projection: GraphProjectionOutcome,
    pub graph_contribution_count: usize,
    pub graph_ready: bool,
    pub pending_summary_refresh: Option<PendingGraphSummaryRefresh>,
}

pub(crate) async fn reconcile_revision_graph(
    state: &AppState,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphReconcileOutcome, GraphServiceError> {
    ensure_not_cancelled(cancellation_token)?;
    let document_row =
        content_repository::get_document_by_id(&state.persistence.postgres, document_id)
            .await
            .with_context(|| format!("failed to load content document {document_id}"))?
            .with_context(|| format!("content document {document_id} not found"))?;
    ensure_not_cancelled(cancellation_token)?;
    let document_head =
        content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .with_context(|| format!("failed to load content document head {document_id}"))?
            .with_context(|| format!("content document head {document_id} not found"))?;
    ensure_not_cancelled(cancellation_token)?;
    let revision = content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
        .await
        .with_context(|| format!("failed to load content revision {revision_id}"))?
        .with_context(|| format!("content revision {revision_id} not found"))?;
    ensure_not_cancelled(cancellation_token)?;
    let revision_chunks =
        content_repository::list_chunks_by_revision(&state.persistence.postgres, revision_id)
            .await
            .with_context(|| format!("failed to list chunks for content revision {revision_id}"))?;
    ensure_not_cancelled(cancellation_token)?;

    let document = synthesize_document_row(&document_row, &document_head, Some(&revision));
    let revision_chunk_ids = revision_chunks.iter().map(|chunk| chunk.id).collect::<BTreeSet<_>>();
    let chunk_rows_by_id = revision_chunks
        .iter()
        .map(|chunk| {
            (chunk.id, synthesize_chunk_row(chunk, document_id, library_id, revision.created_at))
        })
        .collect::<BTreeMap<_, _>>();

    let previous_active_revision_id = document_head
        .active_revision_id
        .filter(|active_revision_id| *active_revision_id != revision_id);
    let (superseded_node_ids, superseded_edge_ids) = collect_superseded_graph_targets(
        state,
        library_id,
        document_id,
        previous_active_revision_id,
        cancellation_token,
    )
    .await?;

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while reconciling revision graph")?;
    ensure_not_cancelled(cancellation_token)?;
    let projection_scope =
        crate::services::graph::projection::resolve_projection_scope(state, library_id)
            .await
            .context("failed to resolve active projection scope for revision graph reconcile")?
            .defer_source_truth_to_lifecycle();
    ensure_not_cancelled(cancellation_token)?;
    let existing_graph_is_empty =
        snapshot.as_ref().is_none_or(|value| value.node_count <= 0 && value.edge_count <= 0);
    if document_row.document_state == "deleted" || document_row.deleted_at.is_some() {
        tracing::info!(
            %library_id,
            %document_id,
            %revision_id,
            "revision graph reconcile skipped because document is deleted"
        );
        let projection = preserve_runtime_graph_snapshot(
            state,
            &projection_scope,
            snapshot,
            "deleted revision graph reconcile",
        )
        .await?;
        return Ok(RevisionGraphReconcileOutcome {
            graph_ready: false,
            graph_contribution_count: 0,
            pending_summary_refresh: None,
            projection,
        });
    }

    let extraction_records = repositories::list_runtime_graph_extraction_records_by_document(
        &state.persistence.postgres,
        document_id,
        GRAPH_EXTRACTION_VERSION,
    )
    .await
    .with_context(|| {
        format!("failed to list graph extraction records for document {document_id}")
    })?;
    let latest_records_by_chunk = select_latest_revision_extraction_records(
        extraction_records,
        &revision_chunk_ids,
        revision_id,
        cancellation_token,
    )?;

    let merge_scope = GraphMergeScope::new(library_id, projection_scope.projection_version)
        .with_lifecycle(Some(revision_id), activated_by_attempt_id);

    let merge_summary = merge_revision_graph_records(
        state,
        document,
        chunk_rows_by_id,
        merge_scope,
        latest_records_by_chunk,
        cancellation_token,
    )
    .await?;
    let graph_contribution_count = merge_summary.graph_contribution_count;
    let changed_node_ids = merge_summary.changed_node_ids;
    let changed_edge_ids = merge_summary.changed_edge_ids;
    // Union the new revision's merge contributions with the superseded
    // revision's now-orphaned targets. This is the exact set of nodes/edges
    // whose support must be re-derived and whose zero-support members must be
    // pruned, and it bounds the projection to this document's graph footprint.
    let targeted_node_ids = union_targeted_ids(&changed_node_ids, &superseded_node_ids);
    let targeted_edge_ids = union_targeted_ids(&changed_edge_ids, &superseded_edge_ids);

    let plan = revision_reconcile_projection_plan(
        previous_active_revision_id.is_some(),
        graph_contribution_count,
        existing_graph_is_empty,
        snapshot.is_some(),
        &targeted_node_ids,
        &targeted_edge_ids,
    );

    let pending_summary_refresh = if plan.broad_summary_refresh() {
        PendingGraphSummaryRefresh::broad()
    } else {
        PendingGraphSummaryRefresh::targeted(changed_node_ids.clone(), changed_edge_ids.clone())
    };

    let projection = execute_revision_reconcile_projection(
        state,
        projection_scope,
        snapshot,
        plan,
        &targeted_node_ids,
        &targeted_edge_ids,
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

#[derive(Debug, Default)]
struct ChunkMergeOutcome {
    contribution: usize,
    node_ids: Vec<Uuid>,
    edge_ids: Vec<Uuid>,
}

struct RevisionGraphMergeSummary {
    graph_contribution_count: usize,
    changed_node_ids: Vec<Uuid>,
    changed_edge_ids: Vec<Uuid>,
}

async fn merge_revision_graph_records(
    state: &AppState,
    document: DocumentRow,
    chunk_rows_by_id: BTreeMap<Uuid, ChunkRow>,
    merge_scope: GraphMergeScope,
    records: BTreeMap<Uuid, repositories::RuntimeGraphExtractionRecordRow>,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphMergeSummary, GraphServiceError> {
    const MERGE_PARALLELISM: usize = 1;
    let pool = state.persistence.postgres.clone();
    let quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let document = Arc::new(document);
    let chunk_rows_by_id = Arc::new(chunk_rows_by_id);
    let merge_scope = Arc::new(merge_scope);
    let merge_results = stream::iter(records.into_values().map(|record| {
        let pool = pool.clone();
        let quality_guard = quality_guard.clone();
        let document = Arc::clone(&document);
        let chunk_rows_by_id = Arc::clone(&chunk_rows_by_id);
        let merge_scope = Arc::clone(&merge_scope);
        let cancellation_token = cancellation_token.clone();
        async move {
            merge_revision_graph_record(
                &pool,
                &quality_guard,
                document,
                chunk_rows_by_id,
                merge_scope,
                record,
                &cancellation_token,
            )
            .await
        }
    }))
    .buffer_unordered(MERGE_PARALLELISM)
    .try_collect::<Vec<_>>()
    .await?;
    let summary = summarize_revision_graph_merges(merge_results, cancellation_token)?;
    reconcile_merge_support_counts(
        &state.persistence.postgres,
        merge_scope.as_ref(),
        &summary.changed_node_ids,
        &summary.changed_edge_ids,
    )
    .await
    .context("failed to reconcile graph support counts during revision graph reconcile")?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(summary)
}

async fn merge_revision_graph_record(
    pool: &sqlx::PgPool,
    quality_guard: &crate::services::graph::quality_guard::GraphQualityGuardService,
    document: Arc<DocumentRow>,
    chunk_rows_by_id: Arc<BTreeMap<Uuid, ChunkRow>>,
    merge_scope: Arc<GraphMergeScope>,
    mut record: repositories::RuntimeGraphExtractionRecordRow,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<ChunkMergeOutcome> {
    ensure_not_cancelled(cancellation_token)?;
    let chunk_id = record.chunk_id;
    let doc_id = document.id;
    let merge_started = std::time::Instant::now();
    tracing::info!(%doc_id, %chunk_id, "graph merge chunk start");
    let Some(chunk_row) = chunk_rows_by_id.get(&chunk_id).cloned() else {
        tracing::info!(%doc_id, %chunk_id, "graph merge chunk skipped — no chunk row");
        return Ok(ChunkMergeOutcome::default());
    };
    if !is_graph_reconcile_chunk_text_eligible(&chunk_row.content) {
        tracing::info!(
            %doc_id,
            %chunk_id,
            elapsed_ms = merge_started.elapsed().as_millis() as u64,
            "graph merge chunk skipped — current chunk text is not graph-eligible"
        );
        return Ok(ChunkMergeOutcome::default());
    }
    let normalized = std::mem::take(&mut record.normalized_output_json);
    let recovery = extraction_recovery_summary_from_record(&record);
    let candidates = tokio::task::spawn_blocking(move || {
        repaired_graph_extraction_candidates(normalized)
    })
    .await
    .context("normalized graph extraction decode task panicked")?
    .with_context(|| {
        format!(
            "failed to decode normalized graph extraction for document {doc_id} chunk {chunk_id}"
        )
    })?;
    ensure_not_cancelled(cancellation_token)?;
    if candidates.entities.is_empty() && candidates.relations.is_empty() {
        tracing::info!(
            %doc_id,
            %chunk_id,
            elapsed_ms = merge_started.elapsed().as_millis() as u64,
            "graph merge chunk done — no candidates"
        );
        return Ok(ChunkMergeOutcome::default());
    }
    merge_nonempty_revision_graph_candidates(
        pool,
        quality_guard,
        &document,
        &chunk_row,
        &merge_scope,
        &candidates,
        recovery.as_ref(),
        cancellation_token,
        merge_started,
    )
    .await
}

async fn merge_nonempty_revision_graph_candidates(
    pool: &sqlx::PgPool,
    quality_guard: &crate::services::graph::quality_guard::GraphQualityGuardService,
    document: &DocumentRow,
    chunk: &ChunkRow,
    merge_scope: &GraphMergeScope,
    candidates: &GraphExtractionCandidateSet,
    recovery: Option<&crate::domains::graph_quality::ExtractionRecoverySummary>,
    cancellation_token: &CancellationToken,
    merge_started: std::time::Instant,
) -> anyhow::Result<ChunkMergeOutcome> {
    const PER_CHUNK_MERGE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);
    let entity_count = candidates.entities.len();
    let relation_count = candidates.relations.len();
    let chunk_id = chunk.id;
    let doc_id = document.id;
    let merge_future = merge_chunk_graph_candidates(
        pool,
        quality_guard,
        merge_scope,
        document,
        chunk,
        candidates,
        recovery,
    );
    let timed_result = tokio::select! {
        () = cancellation_token.cancelled() => {
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = tokio::time::timeout(PER_CHUNK_MERGE_TIMEOUT, merge_future) => result,
    };
    let merge_outcome = timed_result.map_err(|_| {
        tracing::error!(
            %doc_id,
            %chunk_id,
            entity_count,
            relation_count,
            timeout_secs = PER_CHUNK_MERGE_TIMEOUT.as_secs(),
            "graph merge chunk exceeded per-chunk timeout — aborting chunk"
        );
        anyhow::anyhow!(
            "graph merge chunk {chunk_id} exceeded {}s per-chunk timeout on document {doc_id}",
            PER_CHUNK_MERGE_TIMEOUT.as_secs()
        )
    })??;
    tracing::info!(
        %doc_id,
        %chunk_id,
        entity_count,
        relation_count,
        elapsed_ms = merge_started.elapsed().as_millis() as u64,
        contribution = merge_outcome.nodes.len() + merge_outcome.edges.len(),
        "graph merge chunk done"
    );
    Ok(ChunkMergeOutcome {
        contribution: merge_outcome.nodes.len() + merge_outcome.edges.len(),
        node_ids: merge_outcome.summary_refresh_node_ids(),
        edge_ids: merge_outcome.summary_refresh_edge_ids(),
    })
}

fn summarize_revision_graph_merges(
    merge_results: Vec<ChunkMergeOutcome>,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphMergeSummary, GraphServiceError> {
    let mut graph_contribution_count = 0usize;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();
    for outcome in merge_results {
        ensure_not_cancelled(cancellation_token)?;
        graph_contribution_count = graph_contribution_count.saturating_add(outcome.contribution);
        changed_node_ids.extend(outcome.node_ids);
        changed_edge_ids.extend(outcome.edge_ids);
    }
    Ok(RevisionGraphMergeSummary {
        graph_contribution_count,
        changed_node_ids: changed_node_ids.into_iter().collect(),
        changed_edge_ids: changed_edge_ids.into_iter().collect(),
    })
}

async fn collect_superseded_graph_targets(
    state: &AppState,
    library_id: Uuid,
    document_id: Uuid,
    previous_revision_id: Option<Uuid>,
    cancellation_token: &CancellationToken,
) -> Result<(BTreeSet<Uuid>, BTreeSet<Uuid>), GraphServiceError> {
    let Some(previous_revision_id) = previous_revision_id else {
        return Ok((BTreeSet::new(), BTreeSet::new()));
    };
    ensure_not_cancelled(cancellation_token)?;
    repositories::delete_query_execution_references_by_content_revision(
        &state.persistence.postgres,
        library_id,
        document_id,
        previous_revision_id,
    )
    .await
    .with_context(|| {
        format!(
            "failed to delete stale query references for document {document_id} revision {previous_revision_id}"
        )
    })?;
    let targets = repositories::deactivate_runtime_graph_evidence_by_content_revision(
        &state.persistence.postgres,
        library_id,
        document_id,
        previous_revision_id,
    )
    .await
    .with_context(|| {
        format!(
            "failed to deactivate stale graph evidence for document {document_id} revision {previous_revision_id}"
        )
    })?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(partition_graph_target_ids(targets))
}

fn partition_graph_target_ids(
    targets: Vec<repositories::RuntimeGraphEvidenceTargetRow>,
) -> (BTreeSet<Uuid>, BTreeSet<Uuid>) {
    let mut node_ids = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
    for target in targets {
        match target.target_kind.as_str() {
            "node" => {
                node_ids.insert(target.target_id);
            }
            "edge" => {
                edge_ids.insert(target.target_id);
            }
            _ => {}
        }
    }
    (node_ids, edge_ids)
}

fn select_latest_revision_extraction_records(
    records: Vec<repositories::RuntimeGraphExtractionRecordRow>,
    revision_chunk_ids: &BTreeSet<Uuid>,
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
) -> Result<BTreeMap<Uuid, repositories::RuntimeGraphExtractionRecordRow>, GraphServiceError> {
    let mut latest_records = BTreeMap::new();
    for record in records {
        ensure_not_cancelled(cancellation_token)?;
        if extraction_record_matches_revision(&record, revision_chunk_ids, revision_id) {
            latest_records.insert(record.chunk_id, record);
        }
    }
    Ok(latest_records)
}

fn extraction_record_matches_revision(
    record: &repositories::RuntimeGraphExtractionRecordRow,
    revision_chunk_ids: &BTreeSet<Uuid>,
    revision_id: Uuid,
) -> bool {
    if record.status != ASYNC_OP_STATUS_READY || !revision_chunk_ids.contains(&record.chunk_id) {
        return false;
    }
    let lifecycle = extraction_lifecycle_from_record(record);
    lifecycle.revision_id.is_none() || lifecycle.revision_id == Some(revision_id)
}

async fn execute_revision_reconcile_projection(
    state: &AppState,
    projection_scope: GraphProjectionScope,
    snapshot: Option<RuntimeGraphSnapshotRow>,
    plan: RevisionReconcileProjectionPlan,
    targeted_node_ids: &[Uuid],
    targeted_edge_ids: &[Uuid],
    cancellation_token: &CancellationToken,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    match plan {
        RevisionReconcileProjectionPlan::TargetedSupersede => {
            project_targeted_supersede(
                state,
                projection_scope,
                targeted_node_ids,
                targeted_edge_ids,
                cancellation_token,
            )
            .await
        }
        RevisionReconcileProjectionPlan::TargetedFresh => {
            project_targeted_refresh(state, projection_scope, targeted_node_ids, targeted_edge_ids)
                .await
        }
        RevisionReconcileProjectionPlan::Full => project_canonical_graph(state, &projection_scope)
            .await
            .context("failed to project reconciled revision graph")
            .map_err(Into::into),
        RevisionReconcileProjectionPlan::Preserve => {
            preserve_or_empty_projection(
                state,
                &projection_scope,
                snapshot,
                "no-op revision graph reconcile",
            )
            .await
        }
        RevisionReconcileProjectionPlan::Empty => {
            ensure_empty_graph_snapshot(state, &projection_scope)
                .await
                .context("failed to persist empty graph snapshot during no-op revision reconcile")
                .map_err(Into::into)
        }
    }
}

async fn project_targeted_supersede(
    state: &AppState,
    projection_scope: GraphProjectionScope,
    targeted_node_ids: &[Uuid],
    targeted_edge_ids: &[Uuid],
    cancellation_token: &CancellationToken,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    prune_superseded_revision_graph(
        state,
        projection_scope.library_id,
        projection_scope.projection_version,
        targeted_node_ids,
        targeted_edge_ids,
    )
    .await
    .context("failed to prune superseded revision graph before targeted projection")?;
    ensure_not_cancelled(cancellation_token)?;
    project_targeted_refresh(state, projection_scope, targeted_node_ids, targeted_edge_ids).await
}

async fn project_targeted_refresh(
    state: &AppState,
    projection_scope: GraphProjectionScope,
    targeted_node_ids: &[Uuid],
    targeted_edge_ids: &[Uuid],
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let projection_scope = projection_scope
        .with_targeted_refresh(targeted_node_ids.to_vec(), targeted_edge_ids.to_vec());
    project_canonical_graph(state, &projection_scope)
        .await
        .context("failed to project reconciled revision graph")
        .map_err(Into::into)
}

async fn preserve_or_empty_projection(
    state: &AppState,
    projection_scope: &GraphProjectionScope,
    snapshot: Option<RuntimeGraphSnapshotRow>,
    context: &str,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    if snapshot.is_some() {
        return preserve_runtime_graph_snapshot(state, projection_scope, snapshot, context)
            .await
            .map_err(Into::into);
    }
    ensure_empty_graph_snapshot(state, projection_scope)
        .await
        .with_context(|| format!("failed to persist empty graph snapshot during {context}"))
        .map_err(Into::into)
}

/// Projection strategy for one revision-graph reconcile.
///
/// `TargetedSupersede` and `TargetedFresh` both republish only the affected
/// subgraph (bounded by the document's own node/edge footprint); the difference
/// is that a supersede first prunes the old revision's now-orphaned rows.
/// `Full` and `Preserve`/`Empty` keep the prior behavior for the first-ever
/// graph build and genuine no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionReconcileProjectionPlan {
    TargetedSupersede,
    TargetedFresh,
    Full,
    Preserve,
    Empty,
}

impl RevisionReconcileProjectionPlan {
    /// A library-wide source-truth change needs a broad canonical-summary
    /// supersede (the inline regeneration self-skips for large graphs). Targeted
    /// refreshes can scope the summary supersede to their own contributions.
    const fn broad_summary_refresh(self) -> bool {
        matches!(self, Self::TargetedSupersede | Self::Full | Self::Preserve | Self::Empty)
    }
}

/// Decides how to project a revision-graph reconcile.
///
/// The driving goal is that a per-document re-revision must NOT rebuild the
/// whole library projection (the > 2 GiB resident load that OOM-killed the
/// worker). When a previous revision existed we take the targeted supersede
/// path whenever there is anything to change, falling back to preserve/empty
/// only for a genuine no-op so an empty union can never silently route into the
/// full (OOM) rebuild.
const fn revision_reconcile_projection_plan(
    has_previous_revision: bool,
    graph_contribution_count: usize,
    existing_graph_is_empty: bool,
    has_snapshot: bool,
    targeted_node_ids: &[Uuid],
    targeted_edge_ids: &[Uuid],
) -> RevisionReconcileProjectionPlan {
    let has_targets = !targeted_node_ids.is_empty() || !targeted_edge_ids.is_empty();

    if has_previous_revision {
        // Re-revision supersede. The existing graph is already populated for
        // this library, so a targeted refresh over the union of new
        // contributions and superseded orphans is correct and bounded. Only a
        // truly empty union (nothing merged, nothing superseded) is a no-op.
        if has_targets {
            return RevisionReconcileProjectionPlan::TargetedSupersede;
        }
        return if has_snapshot {
            RevisionReconcileProjectionPlan::Preserve
        } else {
            RevisionReconcileProjectionPlan::Empty
        };
    }

    // First activation of this document's revision (no predecessor).
    if existing_graph_is_empty {
        // The library graph is empty/new; a full projection is cheap here and
        // also covers the first-ever build for the library.
        return RevisionReconcileProjectionPlan::Full;
    }
    if graph_contribution_count > 0 && has_targets {
        return RevisionReconcileProjectionPlan::TargetedFresh;
    }
    if has_snapshot {
        RevisionReconcileProjectionPlan::Preserve
    } else {
        RevisionReconcileProjectionPlan::Empty
    }
}

fn union_targeted_ids(changed_ids: &[Uuid], superseded_ids: &BTreeSet<Uuid>) -> Vec<Uuid> {
    let mut union = superseded_ids.clone();
    union.extend(changed_ids.iter().copied());
    union.into_iter().collect()
}

/// Re-derives support counts for `node_ids`/`edge_ids` from surviving active
/// evidence, prunes any that dropped to zero support, drops their orphaned
/// canonical summaries, and mirrors the node/edge deletions into `PostgreSQL`.
///
/// Mirrors `refresh_deleted_library_graph_projection_for_cleanup` step-for-step
/// so the superseded revision's contributions leave no graph trace while shared
/// nodes/edges (still supported by other documents) survive.
async fn prune_superseded_revision_graph(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<(), GraphServiceError> {
    repositories::recalculate_runtime_graph_node_support_counts_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        node_ids,
    )
    .await
    .context("failed to recalculate node support counts for superseded revision prune")?;
    repositories::recalculate_runtime_graph_edge_support_counts_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        edge_ids,
    )
    .await
    .context("failed to recalculate edge support counts for superseded revision prune")?;
    let deleted_edge_keys = repositories::delete_runtime_graph_edges_without_support_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        edge_ids,
    )
    .await
    .context("failed to prune zero-support edges for superseded revision")?;
    let deleted_node_keys = repositories::delete_runtime_graph_nodes_without_support_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        node_ids,
    )
    .await
    .context("failed to prune zero-support nodes for superseded revision")?;
    // Canonical summaries have no FK back to the node/edge tables, so the prune
    // above does not cascade. Drop any summary whose target was just removed.
    repositories::delete_runtime_graph_canonical_summaries_for_orphan_targets(
        &state.persistence.postgres,
        library_id,
        node_ids,
        edge_ids,
    )
    .await
    .context("failed to prune orphan canonical summaries for superseded revision")?;
    if !deleted_edge_keys.is_empty() {
        state
            .graph_store
            .delete_relations_by_canonical_keys(library_id, &deleted_edge_keys)
            .await
            .context("failed to sync superseded relation deletions to PostgreSQL")?;
    }
    if !deleted_node_keys.is_empty() {
        state
            .graph_store
            .delete_entities_by_canonical_keys(library_id, &deleted_node_keys)
            .await
            .context("failed to sync superseded entity deletions to PostgreSQL")?;
    }
    Ok(())
}

fn is_graph_reconcile_chunk_text_eligible(text: &str) -> bool {
    is_graph_extraction_text_eligible(text)
}

fn empty_rebuild_conflict(
    library_id: Uuid,
    snapshot: Option<&RuntimeGraphSnapshotRow>,
    reason: &str,
) -> Option<GraphServiceError> {
    let snapshot = snapshot
        .filter(|snapshot| snapshot.graph_status == GRAPH_STATUS_READY)
        .filter(|snapshot| snapshot.node_count > 0 || snapshot.edge_count > 0)?;
    Some(GraphServiceError::StateConflict {
        message: format!(
            "runtime graph rebuild for library {library_id} would publish an empty projection because {reason}, but active projection {} is ready with {} nodes and {} edges; rebuild source material is inconsistent and the active projection was left unchanged",
            snapshot.projection_version, snapshot.node_count, snapshot.edge_count
        ),
    })
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

#[cfg(test)]
fn count_surviving_documents(records: &[repositories::RuntimeGraphExtractionRecordRow]) -> usize {
    records.iter().map(|record| record.document_id).collect::<BTreeSet<_>>().len()
}

async fn run_rebuild_projection(
    state: &AppState,
    scope: &GraphProjectionScope,
    failure_context: &str,
) -> anyhow::Result<GraphProjectionOutcome> {
    project_canonical_graph(state, scope).await.with_context(|| failure_context.to_string())
}

async fn preserve_runtime_graph_snapshot(
    state: &AppState,
    scope: &GraphProjectionScope,
    snapshot: Option<repositories::RuntimeGraphSnapshotRow>,
    context: &str,
) -> anyhow::Result<GraphProjectionOutcome> {
    if let Some(snapshot) = snapshot {
        persist_runtime_graph_snapshot(
            state,
            scope,
            "building",
            snapshot.node_count,
            snapshot.edge_count,
            Some(snapshot.provenance_coverage_percent.unwrap_or(100.0)),
            None,
        )
        .await
        .with_context(|| format!("failed to claim graph snapshot during {context}"))?;
        persist_runtime_graph_snapshot(
            state,
            scope,
            "ready",
            snapshot.node_count,
            snapshot.edge_count,
            Some(snapshot.provenance_coverage_percent.unwrap_or(100.0)),
            None,
        )
        .await
        .with_context(|| format!("failed to preserve ready graph snapshot during {context}"))?;
        return Ok(GraphProjectionOutcome {
            projection_version: scope.projection_version,
            node_count: usize::try_from(snapshot.node_count).unwrap_or_default(),
            edge_count: usize::try_from(snapshot.edge_count).unwrap_or_default(),
            graph_status: "ready".to_string(),
        });
    }

    ensure_empty_graph_snapshot(state, scope)
        .await
        .with_context(|| format!("failed to persist empty graph snapshot during {context}"))
}

fn synthesize_document_row(
    document_row: &content_repository::ContentDocumentRow,
    document_head: &content_repository::ContentDocumentHeadRow,
    revision: Option<&content_repository::ContentRevisionRow>,
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
    chunk_row: &content_repository::ContentChunkRow,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::repositories::RuntimeGraphExtractionRecordRow;

    #[test]
    fn malformed_persisted_candidate_contract_fails_closed() {
        let error = repaired_graph_extraction_candidates(serde_json::json!({
            "entities": { "unexpected": true },
            "relations": []
        }))
        .expect_err("malformed persisted candidates must not become an empty successful graph");

        assert!(error.to_string().contains("candidate contract"));
    }

    #[test]
    fn counts_unique_documents_in_rebuild_plan() {
        let document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();

        let records = vec![
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract".to_string(),
                prompt_hash: "a".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract".to_string(),
                prompt_hash: "b".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                document_id: other_document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract".to_string(),
                prompt_hash: "c".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
        ];

        assert_eq!(count_surviving_documents(&records), 2);
    }

    #[test]
    fn re_revision_with_changes_takes_targeted_supersede_path() {
        // A re-revision on a populated library (the OOM scenario): there is a
        // previous revision and the existing graph is non-empty. Even though the
        // old full-rebuild trigger fired here, we now route to the bounded
        // targeted supersede path instead of a library-wide rebuild.
        let plan = revision_reconcile_projection_plan(
            true,
            3,
            false,
            true,
            &[Uuid::now_v7()],
            &[Uuid::now_v7()],
        );
        assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedSupersede);
        assert!(plan.broad_summary_refresh());
    }

    #[test]
    fn re_revision_pruning_only_still_targeted_supersede() {
        // Content-shrinking re-revision: the new revision contributes nothing
        // (graph_contribution_count == 0) but the superseded revision left
        // orphan targets that must be pruned. Must NOT fall through to preserve.
        let plan = revision_reconcile_projection_plan(true, 0, false, true, &[Uuid::now_v7()], &[]);
        assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedSupersede);
    }

    #[test]
    fn re_revision_empty_union_preserves_instead_of_full_rebuild() {
        // Genuine no-op re-revision: nothing merged, nothing superseded. The
        // empty union must route to preserve, never silently into the full
        // (OOM) rebuild.
        let plan = revision_reconcile_projection_plan(true, 0, false, true, &[], &[]);
        assert_eq!(plan, RevisionReconcileProjectionPlan::Preserve);
    }

    #[test]
    fn re_revision_empty_union_without_snapshot_is_empty() {
        let plan = revision_reconcile_projection_plan(true, 0, false, false, &[], &[]);
        assert_eq!(plan, RevisionReconcileProjectionPlan::Empty);
    }

    #[test]
    fn first_revision_on_empty_library_is_full() {
        // First-ever graph for the library: full projection is cheap on an
        // empty/new graph.
        let plan = revision_reconcile_projection_plan(
            false,
            5,
            true,
            false,
            &[Uuid::now_v7()],
            &[Uuid::now_v7()],
        );
        assert_eq!(plan, RevisionReconcileProjectionPlan::Full);
    }

    #[test]
    fn first_revision_on_populated_library_is_targeted_fresh() {
        let plan = revision_reconcile_projection_plan(
            false,
            5,
            false,
            true,
            &[Uuid::now_v7()],
            &[Uuid::now_v7()],
        );
        assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedFresh);
        assert!(!plan.broad_summary_refresh());
    }

    #[test]
    fn first_revision_no_contribution_preserves() {
        let plan = revision_reconcile_projection_plan(false, 0, false, true, &[], &[]);
        assert_eq!(plan, RevisionReconcileProjectionPlan::Preserve);
    }

    #[test]
    fn union_targeted_ids_dedups_and_merges() {
        let shared = Uuid::now_v7();
        let only_changed = Uuid::now_v7();
        let only_superseded = Uuid::now_v7();
        let superseded: BTreeSet<Uuid> = [shared, only_superseded].into_iter().collect();
        let union = union_targeted_ids(&[shared, only_changed], &superseded);
        assert_eq!(union.len(), 3);
        assert!(union.contains(&shared));
        assert!(union.contains(&only_changed));
        assert!(union.contains(&only_superseded));
    }

    #[test]
    fn empty_rebuild_conflicts_with_live_snapshot() {
        let library_id = Uuid::now_v7();
        let snapshot = snapshot_row(library_id, GRAPH_STATUS_READY, 7, 12, 34);

        let error = empty_rebuild_conflict(library_id, Some(&snapshot), "no source")
            .expect("live graph must reject empty rebuild");

        assert!(matches!(error, GraphServiceError::StateConflict { .. }));
        assert!(error.to_string().contains("projection 7"));
        assert!(error.to_string().contains("left unchanged"));
    }

    #[test]
    fn empty_rebuild_allows_empty_or_absent_snapshot() {
        let library_id = Uuid::now_v7();
        let empty_snapshot = snapshot_row(library_id, "empty", 3, 0, 0);

        assert!(empty_rebuild_conflict(library_id, None, "no source").is_none());
        assert!(empty_rebuild_conflict(library_id, Some(&empty_snapshot), "no source").is_none());
    }

    #[test]
    fn graph_reconcile_rejects_low_confidence_current_chunk_text() {
        let text = concat!(
            "overview status alpha beta gamma. ",
            "<!-- formula-not-decoded --> ",
            "abCD4efGH hiJKlmNO pQrST uvWXyZab. ",
            "cdEFGh3Ij klMNOprs tuVWxyZq mnOPqRst."
        );

        assert!(!is_graph_reconcile_chunk_text_eligible(text));
    }

    #[test]
    fn graph_reconcile_accepts_code_like_current_chunk_text() {
        let text = concat!(
            "POST /api/v1/projects getProjectById renderHTMLNode ",
            "AUTH_TOKEN_TIMEOUT_MS status_code retry_count"
        );

        assert!(is_graph_reconcile_chunk_text_eligible(text));
    }

    fn snapshot_row(
        library_id: Uuid,
        graph_status: &str,
        projection_version: i64,
        node_count: i32,
        edge_count: i32,
    ) -> RuntimeGraphSnapshotRow {
        RuntimeGraphSnapshotRow {
            library_id,
            graph_status: graph_status.to_string(),
            projection_version,
            topology_generation: 1,
            node_count,
            edge_count,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
