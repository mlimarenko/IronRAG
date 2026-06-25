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
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph::projection::{
            GraphProjectionOutcome, GraphProjectionScope, ensure_empty_graph_snapshot,
            next_projection_version, project_canonical_graph,
        },
        ingest::cancellation::{StageError, ensure_not_cancelled},
    },
    shared::extraction::text_quality::is_graph_extraction_text_eligible,
};

pub async fn rebuild_library_graph(
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
        return ensure_empty_graph_snapshot(state, library_id, projection_version).await;
    }

    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();

    for record in extractions {
        if record.status != ASYNC_OP_STATUS_READY {
            continue;
        }

        let Some(document_row) =
            content_repository::get_document_by_id(&state.persistence.postgres, record.document_id)
                .await
                .with_context(|| format!("failed to load document {}", record.document_id))?
        else {
            continue;
        };
        if document_row.deleted_at.is_some() {
            continue;
        }
        let Some(document_head) =
            content_repository::get_document_head(&state.persistence.postgres, record.document_id)
                .await
                .with_context(|| format!("failed to load document head {}", record.document_id))?
        else {
            continue;
        };
        let extraction_lifecycle = extraction_lifecycle_from_record(&record);
        if extraction_lifecycle.revision_id.is_some()
            && extraction_lifecycle.revision_id != document_head.active_revision_id
        {
            continue;
        }
        let active_revision_id =
            extraction_lifecycle.revision_id.or(document_head.active_revision_id);
        let revision = match active_revision_id {
            Some(revision_id) => {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .with_context(|| format!("failed to load revision {}", revision_id))?
            }
            None => None,
        };
        let Some(chunk_row) =
            content_repository::get_chunk_by_id(&state.persistence.postgres, record.chunk_id)
                .await
                .with_context(|| format!("failed to load chunk {}", record.chunk_id))?
        else {
            continue;
        };
        if !is_graph_reconcile_chunk_text_eligible(&chunk_row.normalized_text) {
            continue;
        }
        let document = DocumentRow {
            id: document_row.id,
            library_id,
            source_id: None,
            external_key: document_row.external_key.clone(),
            title: revision.as_ref().and_then(|value| value.title.clone()),
            mime_type: revision.as_ref().map(|value| value.mime_type.clone()),
            checksum: revision.as_ref().map(|value| value.checksum.clone()),
            active_revision_id: document_head.active_revision_id,
            document_state: document_row.document_state.clone(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: document_row.deleted_at,
            created_at: document_row.created_at,
            updated_at: document_head.head_updated_at,
        };
        let chunk = ChunkRow {
            id: chunk_row.id,
            document_id: document_row.id,
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
            created_at: revision.as_ref().map(|value| value.created_at).unwrap_or_else(Utc::now),
        };
        let candidates =
            repaired_graph_extraction_candidates(record.normalized_output_json.clone());
        if candidates.entities.is_empty() && candidates.relations.is_empty() {
            continue;
        }

        let merge_scope = GraphMergeScope::new(library_id, projection_version)
            .with_lifecycle(active_revision_id, extraction_lifecycle.activated_by_attempt_id);
        let merge_outcome = merge_chunk_graph_candidates(
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
            format!(
                "failed to rebuild graph knowledge for document {} chunk {}",
                document.id, chunk.id
            )
        })?;
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
        return ensure_empty_graph_snapshot(state, library_id, projection_version).await;
    }

    let projection_scope = GraphProjectionScope::new(library_id, projection_version);
    run_rebuild_projection(state, &projection_scope, "failed to project rebuilt graph")
        .await
        .map_err(Into::into)
}

#[derive(Debug, Clone)]
pub struct RevisionGraphReconcileOutcome {
    pub projection: GraphProjectionOutcome,
    pub graph_contribution_count: usize,
    pub graph_ready: bool,
}

pub async fn reconcile_revision_graph(
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
    // Nodes/edges the superseded revision used to support. After its evidence
    // is deleted these may drop to zero support and need pruning; we union them
    // with the new revision's merge contributions so the projection refresh can
    // stay targeted to this document's own graph footprint instead of forcing a
    // library-wide rebuild (the OOM root cause for stuck re-revision jobs).
    let mut superseded_node_ids = BTreeSet::<Uuid>::new();
    let mut superseded_edge_ids = BTreeSet::<Uuid>::new();
    if let Some(previous_active_revision_id) = previous_active_revision_id {
        ensure_not_cancelled(cancellation_token)?;
        repositories::delete_query_execution_references_by_content_revision(
            &state.persistence.postgres,
            library_id,
            document_id,
            previous_active_revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to delete stale query references for document {document_id} revision {previous_active_revision_id}"
            )
        })?;
        let superseded_targets = repositories::deactivate_runtime_graph_evidence_by_content_revision(
            &state.persistence.postgres,
            library_id,
            document_id,
            previous_active_revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to deactivate stale graph evidence for document {document_id} revision {previous_active_revision_id}"
            )
        })?;
        for target in superseded_targets {
            match target.target_kind.as_str() {
                "node" => {
                    superseded_node_ids.insert(target.target_id);
                }
                "edge" => {
                    superseded_edge_ids.insert(target.target_id);
                }
                _ => {}
            }
        }
        ensure_not_cancelled(cancellation_token)?;
    }

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while reconciling revision graph")?;
    ensure_not_cancelled(cancellation_token)?;
    let mut projection_scope =
        crate::services::graph::projection::resolve_projection_scope(state, library_id)
            .await
            .context("failed to resolve active projection scope for revision graph reconcile")?;
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
            library_id,
            projection_scope.projection_version,
            snapshot,
            "deleted revision graph reconcile",
        )
        .await?;
        return Ok(RevisionGraphReconcileOutcome {
            graph_ready: false,
            graph_contribution_count: 0,
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
    let mut latest_records_by_chunk =
        BTreeMap::<Uuid, repositories::RuntimeGraphExtractionRecordRow>::new();
    for record in extraction_records {
        ensure_not_cancelled(cancellation_token)?;
        if record.status != ASYNC_OP_STATUS_READY || !revision_chunk_ids.contains(&record.chunk_id)
        {
            continue;
        }
        let extraction_lifecycle = extraction_lifecycle_from_record(&record);
        if extraction_lifecycle.revision_id.is_some()
            && extraction_lifecycle.revision_id != Some(revision_id)
        {
            continue;
        }
        latest_records_by_chunk.insert(record.chunk_id, record);
    }

    let merge_scope = GraphMergeScope::new(library_id, projection_scope.projection_version)
        .with_lifecycle(Some(revision_id), activated_by_attempt_id);

    // Each per-chunk future captures only what it needs through `Arc`-ed
    // shared state to keep capture cost down (the postgres pool clones
    // cheaply, but `DocumentRow` and the GraphQualityGuardService get one
    // explicit `Arc` apiece). We also consume `latest_records_by_chunk` by
    // value via `into_values()` and `mem::take` the heavy
    // `normalized_output_json` `serde_json::Value` straight into the
    // deserializer — eliminating the per-chunk deep clone that dominates
    // allocator pressure on documents with many chunks.
    //
    // Keep the database merge sequential inside one revision. Different
    // chunks routinely emit the same canonical entity keys, and concurrent
    // `ON CONFLICT DO UPDATE` batches can deadlock while taking row locks in
    // different orders. Extraction still happens before this step; this
    // serialization only covers the canonical graph merge. Revisit only with
    // a single canonical lock-ordering or revision-wide aggregation design.
    const MERGE_PARALLELISM: usize = 1;
    let pool = state.persistence.postgres.clone();
    let quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let document_arc = Arc::new(document.clone());
    let chunk_rows_by_id_arc = Arc::new(chunk_rows_by_id);
    let merge_scope = Arc::new(merge_scope);

    #[derive(Debug, Default)]
    struct ChunkMergeOutcome {
        contribution: usize,
        node_ids: Vec<Uuid>,
        edge_ids: Vec<Uuid>,
    }

    let merge_results = stream::iter(latest_records_by_chunk.into_values().map(|record| {
        let pool = pool.clone();
        let quality_guard = quality_guard.clone();
        let document = Arc::clone(&document_arc);
        let chunk_rows_by_id = Arc::clone(&chunk_rows_by_id_arc);
        let merge_scope = Arc::clone(&merge_scope);
        let cancellation_token = cancellation_token.clone();
        let doc_id = document_arc.id;
        async move {
            ensure_not_cancelled(&cancellation_token)?;
            let chunk_id = record.chunk_id;
            let merge_started = std::time::Instant::now();
            // Per-chunk entry trace so the next hot-stuck incident can
            // be traced down to the exact chunk id that entered merge
            // but never exited. When the worker goes CPU-dead we lose
            // visibility from that point on, so logging entry + exit
            // with elapsed gives the "last known good chunk" needed to
            // isolate the bad payload later.
            tracing::info!(%doc_id, %chunk_id, "graph merge chunk start");
            let Some(chunk_row) = chunk_rows_by_id.get(&chunk_id).cloned() else {
                tracing::info!(
                    %doc_id,
                    %chunk_id,
                    "graph merge chunk skipped — no chunk row"
                );
                return Ok::<ChunkMergeOutcome, anyhow::Error>(ChunkMergeOutcome::default());
            };
            if !is_graph_reconcile_chunk_text_eligible(&chunk_row.content) {
                tracing::info!(
                    %doc_id,
                    %chunk_id,
                    elapsed_ms = merge_started.elapsed().as_millis() as u64,
                    "graph merge chunk skipped — current chunk text is not graph-eligible"
                );
                return Ok::<ChunkMergeOutcome, anyhow::Error>(ChunkMergeOutcome::default());
            }
            let mut record = record;
            let normalized = std::mem::take(&mut record.normalized_output_json);
            let recovery = extraction_recovery_summary_from_record(&record);
            // Large LLM normalized outputs can make this
            // `serde_json::from_value` into a multi-megabyte CPU-bound
            // deserialization. Running it inside `buffer_unordered`
            // on the tokio worker threads is enough to starve the
            // heartbeat/cancel tasks on a small runtime. Offload to the
            // blocking pool so the async runtime keeps servicing
            // control-plane traffic while the deserializer works.
            let candidates = tokio::task::spawn_blocking(move || {
                repaired_graph_extraction_candidates(normalized)
            })
            .await
            .unwrap_or_default();
            ensure_not_cancelled(&cancellation_token)?;
            if candidates.entities.is_empty() && candidates.relations.is_empty() {
                tracing::info!(
                    %doc_id,
                    %chunk_id,
                    elapsed_ms = merge_started.elapsed().as_millis() as u64,
                    "graph merge chunk done — no candidates"
                );
                return Ok(ChunkMergeOutcome::default());
            }
            let entity_count = candidates.entities.len();
            let relation_count = candidates.relations.len();
            // Wall-clock cap per chunk. If the merge body spins for
            // longer than this, abort the chunk (the chunk-level
            // failure degrades to an ingest error at the outer layer).
            // This is an additional safety net on top of the stage
            // timeout — that one can itself starve if the runtime is
            // saturated, but the `tokio::time::timeout` combinator
            // still fires eventually once this future gets polled.
            const PER_CHUNK_MERGE_TIMEOUT: std::time::Duration =
                std::time::Duration::from_secs(180);
            let merge_fut = merge_chunk_graph_candidates(
                &pool,
                &quality_guard,
                &merge_scope,
                document.as_ref(),
                &chunk_row,
                &candidates,
                recovery.as_ref(),
            );
            let merge_outcome = match tokio::select! {
                _ = cancellation_token.cancelled() => {
                    return Err(anyhow::Error::new(StageError::Cancelled));
                }
                result = tokio::time::timeout(PER_CHUNK_MERGE_TIMEOUT, merge_fut) => result,
            } {
                Ok(result) => result.with_context(|| {
                    format!(
                        "failed to merge graph candidates for document {} chunk {}",
                        document.id, chunk_id
                    )
                })?,
                Err(_) => {
                    tracing::error!(
                        %doc_id,
                        %chunk_id,
                        entity_count,
                        relation_count,
                        timeout_secs = PER_CHUNK_MERGE_TIMEOUT.as_secs(),
                        "graph merge chunk exceeded per-chunk timeout — aborting chunk"
                    );
                    return Err(anyhow::anyhow!(
                        "graph merge chunk {chunk_id} exceeded {}s per-chunk timeout on document {}",
                        PER_CHUNK_MERGE_TIMEOUT.as_secs(),
                        document.id
                    ));
                }
            };
            let elapsed_ms = merge_started.elapsed().as_millis() as u64;
            tracing::info!(
                %doc_id,
                %chunk_id,
                entity_count,
                relation_count,
                elapsed_ms,
                contribution = merge_outcome.nodes.len() + merge_outcome.edges.len(),
                "graph merge chunk done"
            );
            Ok(ChunkMergeOutcome {
                contribution: merge_outcome.nodes.len() + merge_outcome.edges.len(),
                node_ids: merge_outcome.summary_refresh_node_ids().into_iter().collect(),
                edge_ids: merge_outcome.summary_refresh_edge_ids().into_iter().collect(),
            })
        }
    }))
    .buffer_unordered(MERGE_PARALLELISM)
    .try_collect::<Vec<_>>()
    .await?;

    let mut graph_contribution_count = 0usize;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();
    for outcome in merge_results {
        ensure_not_cancelled(cancellation_token)?;
        graph_contribution_count = graph_contribution_count.saturating_add(outcome.contribution);
        changed_node_ids.extend(outcome.node_ids);
        changed_edge_ids.extend(outcome.edge_ids);
    }

    reconcile_merge_support_counts(
        &state.persistence.postgres,
        merge_scope.as_ref(),
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to reconcile graph support counts during revision graph reconcile")?;
    ensure_not_cancelled(cancellation_token)?;

    let changed_edge_ids = changed_edge_ids.into_iter().collect::<Vec<_>>();
    let changed_node_ids = changed_node_ids.into_iter().collect::<Vec<_>>();
    let source_truth_version =
        crate::services::query::support::invalidate_library_source_truth(state, library_id)
            .await
            .context("failed to advance source truth during revision graph reconcile")?;
    ensure_not_cancelled(cancellation_token)?;

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

    let summary_refresh = if plan.broad_summary_refresh() {
        crate::services::graph::summary::GraphSummaryRefreshRequest::broad()
    } else {
        crate::services::graph::summary::GraphSummaryRefreshRequest::targeted(
            changed_node_ids.clone(),
            changed_edge_ids.clone(),
        )
    }
    .with_source_truth_version(source_truth_version);
    projection_scope = projection_scope.with_summary_refresh(summary_refresh);

    let projection = match plan {
        RevisionReconcileProjectionPlan::TargetedSupersede => {
            // Prune the superseded revision's now-orphaned nodes/edges before
            // the targeted projection republishes, mirroring the
            // document-delete cleanup contract. Without this the old revision's
            // entities stay alive via stale support counts.
            prune_superseded_revision_graph(
                state,
                library_id,
                projection_scope.projection_version,
                &targeted_node_ids,
                &targeted_edge_ids,
            )
            .await
            .context("failed to prune superseded revision graph before targeted projection")?;
            ensure_not_cancelled(cancellation_token)?;
            projection_scope = projection_scope
                .with_targeted_refresh(targeted_node_ids.clone(), targeted_edge_ids.clone());
            project_canonical_graph(state, &projection_scope)
                .await
                .context("failed to project reconciled revision graph")?
        }
        RevisionReconcileProjectionPlan::TargetedFresh => {
            projection_scope = projection_scope
                .with_targeted_refresh(targeted_node_ids.clone(), targeted_edge_ids.clone());
            project_canonical_graph(state, &projection_scope)
                .await
                .context("failed to project reconciled revision graph")?
        }
        RevisionReconcileProjectionPlan::Full => project_canonical_graph(state, &projection_scope)
            .await
            .context("failed to project reconciled revision graph")?,
        RevisionReconcileProjectionPlan::Preserve => {
            // Safe to unwrap conceptually: the plan only selects Preserve when a
            // snapshot exists, but stay defensive against a snapshot that
            // disappeared between resolve and here.
            if let Some(snapshot) = snapshot {
                preserve_runtime_graph_snapshot(
                    state,
                    library_id,
                    projection_scope.projection_version,
                    Some(snapshot),
                    "no-op revision graph reconcile",
                )
                .await?
            } else {
                ensure_empty_graph_snapshot(state, library_id, projection_scope.projection_version)
                    .await
                    .context(
                        "failed to persist empty graph snapshot during no-op revision reconcile",
                    )?
            }
        }
        RevisionReconcileProjectionPlan::Empty => {
            ensure_empty_graph_snapshot(state, library_id, projection_scope.projection_version)
                .await
                .context("failed to persist empty graph snapshot during no-op revision reconcile")?
        }
    };

    Ok(RevisionGraphReconcileOutcome {
        graph_ready: graph_contribution_count > 0 && projection.graph_status == GRAPH_STATUS_READY,
        graph_contribution_count,
        projection,
    })
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
fn revision_reconcile_projection_plan(
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
/// canonical summaries, and mirrors the node/edge deletions into PostgreSQL.
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
) -> GraphExtractionCandidateSet {
    serde_json::from_value::<GraphExtractionCandidateSet>(repair_graph_extraction_normalized_json(
        normalized_output_json,
    ))
    .map(repair_graph_extraction_candidate_set)
    .unwrap_or_default()
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
    library_id: Uuid,
    projection_version: i64,
    snapshot: Option<repositories::RuntimeGraphSnapshotRow>,
    context: &str,
) -> anyhow::Result<GraphProjectionOutcome> {
    if let Some(snapshot) = snapshot {
        repositories::upsert_runtime_graph_snapshot(
            &state.persistence.postgres,
            library_id,
            "ready",
            projection_version,
            snapshot.node_count,
            snapshot.edge_count,
            Some(snapshot.provenance_coverage_percent.unwrap_or(100.0)),
            None,
        )
        .await
        .with_context(|| format!("failed to preserve ready graph snapshot during {context}"))?;
        return Ok(GraphProjectionOutcome {
            projection_version,
            node_count: usize::try_from(snapshot.node_count).unwrap_or_default(),
            edge_count: usize::try_from(snapshot.edge_count).unwrap_or_default(),
            graph_status: "ready".to_string(),
        });
    }

    ensure_empty_graph_snapshot(state, library_id, projection_version)
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
