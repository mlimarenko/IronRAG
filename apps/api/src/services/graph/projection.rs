use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, anyhow};
use sqlx::PgConnection;
use tokio::time::sleep;
use uuid::Uuid;

use crate::domains::ops::{GRAPH_STATUS_EMPTY, GRAPH_STATUS_READY};
use crate::services::graph::projection_guard::GraphWriteFailureDecision;
use crate::{
    app::state::AppState,
    infra::{
        knowledge_plane::LibraryProjectionTargets,
        knowledge_rows::{
            GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
            sanitize_graph_view_writes,
        },
        repositories::{self, RuntimeGraphSnapshotRow, content_document_list_repository},
    },
    services::graph::{error::GraphServiceError, quality_guard::GraphQualityGuardService},
    services::knowledge::graph_stream::prewarm_graph_topology_cache,
    shared::json_coercion::from_value_or_default,
};

#[derive(Debug)]
pub struct GraphProjectionScope {
    pub library_id: Uuid,
    pub projection_version: i64,
    pub build_epoch: Uuid,
    pub targeted_node_ids: Vec<Uuid>,
    pub targeted_edge_ids: Vec<Uuid>,
    pub source_truth_publication: GraphSourceTruthPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSourceTruthPublication {
    Immediate,
    DeferredToLifecycle,
}

#[derive(Debug, Clone)]
pub struct GraphProjectionOutcome {
    pub projection_version: i64,
    pub node_count: usize,
    pub edge_count: usize,
    pub graph_status: String,
}

#[derive(Debug)]
struct ProjectionWrites {
    nodes: Vec<GraphViewNodeWrite>,
    edges: Vec<GraphViewEdgeWrite>,
    remove_node_ids: Vec<Uuid>,
    remove_edge_ids: Vec<Uuid>,
    projection_pruned_node_ids: Vec<Uuid>,
    projection_pruned_edge_ids: Vec<Uuid>,
}

impl GraphProjectionOutcome {
    #[must_use]
    pub fn has_materialized_graph(&self) -> bool {
        self.graph_status == GRAPH_STATUS_READY && (self.node_count > 0 || self.edge_count > 0)
    }
}

impl GraphProjectionScope {
    #[must_use]
    pub fn new(library_id: Uuid, projection_version: i64) -> Self {
        Self {
            library_id,
            projection_version,
            build_epoch: Uuid::now_v7(),
            targeted_node_ids: Vec::new(),
            targeted_edge_ids: Vec::new(),
            source_truth_publication: GraphSourceTruthPublication::Immediate,
        }
    }

    #[must_use]
    pub const fn defer_source_truth_to_lifecycle(mut self) -> Self {
        self.source_truth_publication = GraphSourceTruthPublication::DeferredToLifecycle;
        self
    }

    #[must_use]
    pub fn with_targeted_refresh(
        mut self,
        mut targeted_node_ids: Vec<Uuid>,
        mut targeted_edge_ids: Vec<Uuid>,
    ) -> Self {
        targeted_node_ids.sort_unstable();
        targeted_node_ids.dedup();
        targeted_edge_ids.sort_unstable();
        targeted_edge_ids.dedup();
        self.targeted_node_ids = targeted_node_ids;
        self.targeted_edge_ids = targeted_edge_ids;
        self
    }

    #[must_use]
    pub const fn is_targeted_refresh(&self) -> bool {
        !self.targeted_node_ids.is_empty() || !self.targeted_edge_ids.is_empty()
    }
}

#[must_use]
pub(crate) fn active_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1)
}

#[must_use]
pub(crate) fn next_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map_or(1, |_| active_projection_version(snapshot) + 1)
}

pub(crate) async fn resolve_projection_scope(
    state: &AppState,
    library_id: Uuid,
) -> Result<GraphProjectionScope, GraphServiceError> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while resolving projection scope")?;
    Ok(GraphProjectionScope::new(library_id, active_projection_version(snapshot.as_ref())))
}

pub(crate) async fn ensure_empty_graph_snapshot(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status: "building",
            node_count: 0,
            edge_count: 0,
            provenance_coverage_percent: Some(0.0),
            last_error_message: None,
        },
    )
    .await?;
    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status: "empty",
            node_count: 0,
            edge_count: 0,
            provenance_coverage_percent: Some(0.0),
            last_error_message: None,
        },
    )
    .await?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: 0,
        edge_count: 0,
        graph_status: "empty".to_string(),
    })
}

/// Payload for one runtime graph snapshot write.
///
/// Groups the status/count/coverage/error fields shared by the pool and
/// executor snapshot writers so both entry points stay within the argument
/// budget; every field is written verbatim.
pub(super) struct GraphSnapshotState<'a> {
    pub(super) graph_status: &'a str,
    pub(super) node_count: i32,
    pub(super) edge_count: i32,
    pub(super) provenance_coverage_percent: Option<f64>,
    pub(super) last_error_message: Option<&'a str>,
}

pub(super) async fn persist_runtime_graph_snapshot(
    state: &AppState,
    scope: &GraphProjectionScope,
    snapshot: GraphSnapshotState<'_>,
) -> Result<RuntimeGraphSnapshotRow, GraphServiceError> {
    let GraphSnapshotState {
        graph_status,
        node_count,
        edge_count,
        provenance_coverage_percent,
        last_error_message,
    } = snapshot;
    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        repositories::RuntimeGraphSnapshotInput {
            library_id: scope.library_id,
            graph_status,
            projection_version: scope.projection_version,
            build_epoch: scope.build_epoch,
            node_count,
            edge_count,
            provenance_coverage_percent,
            last_error_message,
        },
        scope.source_truth_publication == GraphSourceTruthPublication::Immediate,
    )
    .await?
    .ok_or_else(|| GraphServiceError::StateConflict {
        message: format!(
            "runtime graph snapshot publish conflict for library {} projection {} build {}",
            scope.library_id, scope.projection_version, scope.build_epoch
        ),
    })
}

pub(crate) async fn project_normalized_graph(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    // `synchronize_projection_support_counts` runs three library-wide
    // sweeps (recalculate support counts, prune zero-support edges,
    // prune zero-support nodes). On a mid-sized graph (27k edges / 10k
    // nodes / 37k active graph summaries) that sweep costs ~5s. Running
    // it after **every** chunk merge is O(chunks × graph-size) and
    // dominates worker throughput on any non-trivial library.
    //
    // For a targeted refresh (one chunk's contribution) the incident
    // nodes/edges we just upserted cannot create new zero-support
    // orphans anywhere OUTSIDE their own subgraph — support counts
    // only drop when a source_truth_version is superseded, and that
    // happens during full projections. So the sync is safe to skip on
    // targeted paths and run only on full rebuilds.
    if scope.is_targeted_refresh() {
        return project_targeted_normalized_graph(state, scope).await.map_err(Into::into);
    }
    synchronize_projection_support_counts(state, scope).await?;
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load active graph nodes for projection")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load active graph edges for projection")?;

    if nodes.is_empty() && edges.is_empty() {
        return ensure_empty_graph_snapshot(state, scope).await;
    }

    let provenance_coverage_percent =
        provenance_coverage_percent(state, scope.library_id, scope.projection_version).await?;
    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status: "building",
            node_count: i32::try_from(nodes.len()).unwrap_or(i32::MAX),
            edge_count: i32::try_from(edges.len()).unwrap_or(i32::MAX),
            provenance_coverage_percent: Some(provenance_coverage_percent),
            last_error_message: None,
        },
    )
    .await
    .context("failed to mark graph snapshot as building")?;
    // Intentionally DO NOT invalidate the topology cache here. The
    // `building` transition advertises work in flight but the previous
    // `ready` snapshot (same projection_version as the cache key) is
    // still the published active graph until the next `ready` upsert
    // below swaps it out. Dropping the cache here caused every ingest
    // cycle on a library to DEL the only live key before the replacement
    // landed, forcing every concurrent GET to rebuild from Postgres and
    // producing 25 s cold-path storms on reference libraries.

    // Node/edge write construction + sanitization is the single biggest
    // synchronous CPU hot spot of the extract_graph stage: for a prod
    // reference library (25k nodes, 80k edges) this clones ~105k rows with
    // `serde_json::Value` payloads, sorts 80k edges, and walks a 25k
    // BTreeSet — all on the tokio worker thread. Hand it to
    // `spawn_blocking` so the runtime can service the heartbeat and
    // cancel poll tasks while the CPU work happens on the blocking
    // thread pool. The move-in transfers ownership of the freshly
    // loaded rows; the move-out returns the sanitized writes.
    let projection_quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let sanitize_task = tokio::task::spawn_blocking(move || {
        projection_writes_from_rows(&projection_quality_guard, &nodes, &edges)
    });
    let projection_writes =
        sanitize_task.await.context("graph projection sanitize task panicked")?;
    prune_disallowed_projection_rows(state, scope, &projection_writes).await?;

    if let Err(error) =
        execute_projection_write_with_guard(state, scope, "library_projection", || {
            state.graph_store.replace_library_projection(
                scope.library_id,
                scope.projection_version,
                &projection_writes.nodes,
                &projection_writes.edges,
            )
        })
        .await
    {
        let failure_message = error.to_string();
        persist_runtime_graph_snapshot(
            state,
            scope,
            GraphSnapshotState {
                graph_status: "failed",
                node_count: i32::try_from(projection_writes.nodes.len()).unwrap_or(i32::MAX),
                edge_count: i32::try_from(projection_writes.edges.len()).unwrap_or(i32::MAX),
                provenance_coverage_percent: Some(provenance_coverage_percent),
                last_error_message: Some(&failure_message),
            },
        )
        .await
        .context("failed to mark graph snapshot as failed after graph-store refresh error")?;
        return Err(error.context("failed to refresh the active graph view").into());
    }

    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status: "ready",
            node_count: i32::try_from(projection_writes.nodes.len()).unwrap_or(i32::MAX),
            edge_count: i32::try_from(projection_writes.edges.len()).unwrap_or(i32::MAX),
            provenance_coverage_percent: Some(provenance_coverage_percent),
            last_error_message: None,
        },
    )
    .await
    .context("failed to mark graph snapshot as ready")?;
    // No explicit invalidate: `schedule_topology_prewarm` rebuilds the
    // NDJSON and writes it with `SET EX` under the fresh
    // `graph:{library_id}:v{projection_version}:g{topology_generation}`
    // key. The generation is advanced by the snapshot upsert, so new
    // readers naturally miss old bytes while in-flight readers can keep
    // using the previous key until its TTL expires.
    schedule_topology_prewarm(state, scope.library_id);
    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: projection_writes.nodes.len(),
        edge_count: projection_writes.edges.len(),
        graph_status: "ready".to_string(),
    })
}

/// Projects a full-library rebuild inside the caller's transaction.
///
/// The caller must already hold both library graph advisory locks and must
/// supply a `REPEATABLE READ` connection. This routine neither opens another
/// transaction nor acquires or releases locks. Any error is left for the
/// caller to roll back, including the non-terminal snapshot claim.
pub(crate) async fn project_rebuild_normalized_graph_with_connection(
    state: &AppState,
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    synchronize_rebuilt_graph_support_with_connection(connection, scope).await?;
    let projection_writes = load_rebuilt_graph_projection_writes(state, connection, scope).await?;

    publish_rebuilt_graph_projection(state, connection, scope, &projection_writes).await
}

/// Synchronizes rebuilt-graph support counts and logs the prune outcome.
///
/// Kept separate so the projection entry point keeps its side effects in the
/// same order without carrying the tracing macro's complexity.
async fn synchronize_rebuilt_graph_support_with_connection(
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
) -> Result<(), GraphServiceError> {
    let support_prune = repositories::synchronize_runtime_graph_support_and_prune_with_connection(
        connection,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to atomically synchronize rebuilt graph support")?;
    tracing::debug!(
        library_id = %scope.library_id,
        deleted_nodes = support_prune.deleted_node_count,
        deleted_edges = support_prune.deleted_edge_count,
        deleted_summaries = support_prune.deleted_summary_count,
        "atomically synchronized rebuilt graph support",
    );
    Ok(())
}

/// Loads, sanitizes and prunes the writes for a rebuilt graph projection.
async fn load_rebuilt_graph_projection_writes(
    state: &AppState,
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
) -> Result<ProjectionWrites, GraphServiceError> {
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_library_with_executor(
        &mut *connection,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load rebuilt graph nodes for projection")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_library_with_executor(
        &mut *connection,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load rebuilt graph edges for projection")?;

    let projection_quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let sanitize_task = tokio::task::spawn_blocking(move || {
        projection_writes_from_rows(&projection_quality_guard, &nodes, &edges)
    });
    let projection_writes =
        sanitize_task.await.context("rebuilt graph projection sanitize task panicked")?;
    prune_disallowed_projection_rows_with_connection(connection, scope, &projection_writes).await?;
    Ok(projection_writes)
}

/// Publishes the rebuilt graph projection and its terminal snapshot.
async fn publish_rebuilt_graph_projection(
    state: &AppState,
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
    projection_writes: &ProjectionWrites,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let provenance_coverage_percent = provenance_coverage_percent_with_connection(
        connection,
        scope.library_id,
        scope.projection_version,
    )
    .await?;
    let node_count = projection_writes.nodes.len();
    let edge_count = projection_writes.edges.len();
    let snapshot_node_count = i32::try_from(node_count).unwrap_or(i32::MAX);
    let snapshot_edge_count = i32::try_from(edge_count).unwrap_or(i32::MAX);
    let graph_status = rebuilt_graph_status(node_count, edge_count);

    persist_rebuild_runtime_graph_snapshot_with_connection(
        connection,
        scope,
        "building",
        snapshot_node_count,
        snapshot_edge_count,
        provenance_coverage_percent,
    )
    .await
    .context("failed to claim rebuilt graph snapshot")?;

    execute_projection_write_retries(state, || {
        state.graph_store.replace_library_projection(
            scope.library_id,
            scope.projection_version,
            &projection_writes.nodes,
            &projection_writes.edges,
        )
    })
    .await
    .context("failed to refresh the rebuilt graph view")?;

    persist_rebuild_runtime_graph_snapshot_with_connection(
        connection,
        scope,
        graph_status,
        snapshot_node_count,
        snapshot_edge_count,
        provenance_coverage_percent,
    )
    .await
    .context("failed to publish rebuilt graph snapshot")?;

    advance_source_truth_after_rebuild(connection, scope).await?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count,
        edge_count,
        graph_status: graph_status.to_string(),
    })
}

/// Advances the library source-truth version when the rebuild owns publication.
async fn advance_source_truth_after_rebuild(
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
) -> Result<(), GraphServiceError> {
    if scope.source_truth_publication == GraphSourceTruthPublication::Immediate {
        repositories::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *connection,
            scope.library_id,
        )
        .await
        .context("failed to advance source truth after rebuilt graph publication")?;
    }
    Ok(())
}

#[must_use]
const fn rebuilt_graph_status(node_count: usize, edge_count: usize) -> &'static str {
    if node_count == 0 && edge_count == 0 { GRAPH_STATUS_EMPTY } else { GRAPH_STATUS_READY }
}

fn projection_writes_from_rows(
    quality_guard: &GraphQualityGuardService,
    nodes: &[repositories::RuntimeGraphNodeRow],
    edges: &[repositories::RuntimeGraphEdgeRow],
) -> ProjectionWrites {
    let node_writes = nodes
        .iter()
        .map(|node| GraphViewNodeWrite {
            node_id: node.id,
            identity_key: node.identity_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: from_value_or_default("runtime_graph_node.aliases_json", &node.aliases_json),
            metadata_json: node.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let input_node_ids = node_writes.iter().map(|node| node.node_id).collect::<BTreeSet<_>>();
    let projection_pruned_node_ids = node_writes
        .iter()
        .filter(|node| !quality_guard.allows_node(&node.node_type, &node.label))
        .map(|node| node.node_id)
        .collect::<BTreeSet<_>>();
    let edge_writes = edges
        .iter()
        .map(|edge| GraphViewEdgeWrite {
            edge_id: edge.id,
            from_node_id: edge.from_node_id,
            to_node_id: edge.to_node_id,
            relation_type: edge.relation_type.clone(),
            identity_key: edge.identity_key.clone(),
            support_count: edge.support_count,
            summary: edge.summary.clone(),
            weight: edge.weight,
            metadata_json: edge.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let input_edge_ids = edge_writes.iter().map(|edge| edge.edge_id).collect::<BTreeSet<_>>();
    let projection_pruned_edge_ids = edge_writes
        .iter()
        .filter(|edge| {
            projection_pruned_node_ids.contains(&edge.from_node_id)
                || projection_pruned_node_ids.contains(&edge.to_node_id)
        })
        .map(|edge| edge.edge_id)
        .collect::<Vec<_>>();
    let filtered =
        quality_guard.filter_projection(&GraphViewData { nodes: node_writes, edges: edge_writes });
    let (nodes, edges, _skipped_edge_count) =
        sanitize_graph_view_writes(&filtered.nodes, &filtered.edges);
    let retained_node_ids = nodes.iter().map(|node| node.node_id).collect::<BTreeSet<_>>();
    let retained_edge_ids = edges.iter().map(|edge| edge.edge_id).collect::<BTreeSet<_>>();
    let remove_node_ids =
        input_node_ids.difference(&retained_node_ids).copied().collect::<Vec<_>>();
    let remove_edge_ids =
        input_edge_ids.difference(&retained_edge_ids).copied().collect::<Vec<_>>();
    ProjectionWrites {
        nodes,
        edges,
        remove_node_ids,
        remove_edge_ids,
        projection_pruned_node_ids: projection_pruned_node_ids.into_iter().collect(),
        projection_pruned_edge_ids,
    }
}

async fn prune_disallowed_projection_rows(
    state: &AppState,
    scope: &GraphProjectionScope,
    projection_writes: &ProjectionWrites,
) -> Result<(), GraphServiceError> {
    if projection_writes.projection_pruned_node_ids.is_empty()
        && projection_writes.projection_pruned_edge_ids.is_empty()
    {
        return Ok(());
    }

    let outcome = repositories::prune_runtime_graph_projection_rows(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &projection_writes.projection_pruned_node_ids,
        &projection_writes.projection_pruned_edge_ids,
    )
    .await
    .context("failed to atomically prune disallowed graph projection rows")?;
    trace_disallowed_projection_prune(scope, &outcome);
    Ok(())
}

async fn prune_disallowed_projection_rows_with_connection(
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
    projection_writes: &ProjectionWrites,
) -> Result<(), GraphServiceError> {
    // A whole-library rebuild commits the normalized tables as the new source
    // projection, so keep them identical to the sanitized graph-store payload.
    // Unlike a targeted refresh, it is safe to remove every rejected row,
    // including relation-quality failures and the non-document nodes they
    // orphan. The ordinary/targeted path deliberately retains view-only
    // orphans and continues to use the narrower quality-prune sets above.
    if projection_writes.remove_node_ids.is_empty() && projection_writes.remove_edge_ids.is_empty()
    {
        return Ok(());
    }

    let outcome = repositories::prune_runtime_graph_projection_rows_with_connection(
        connection,
        scope.library_id,
        scope.projection_version,
        &projection_writes.remove_node_ids,
        &projection_writes.remove_edge_ids,
    )
    .await
    .context("failed to atomically prune disallowed rebuilt graph projection rows")?;
    trace_disallowed_projection_prune(scope, &outcome);
    Ok(())
}

fn trace_disallowed_projection_prune(
    scope: &GraphProjectionScope,
    outcome: &repositories::RuntimeGraphProjectionPruneOutcome,
) {
    tracing::debug!(
        library_id = %scope.library_id,
        deleted_nodes = outcome.deleted_node_count,
        deleted_edges = outcome.deleted_edge_count,
        deleted_summaries = outcome.deleted_summary_count,
        "atomically pruned disallowed graph projection rows",
    );
}

async fn persist_rebuild_runtime_graph_snapshot_with_connection(
    connection: &mut PgConnection,
    scope: &GraphProjectionScope,
    graph_status: &str,
    node_count: i32,
    edge_count: i32,
    provenance_coverage_percent: f64,
) -> Result<(), GraphServiceError> {
    repositories::upsert_runtime_graph_snapshot_with_executor(
        &mut *connection,
        repositories::RuntimeGraphSnapshotInput {
            library_id: scope.library_id,
            graph_status,
            projection_version: scope.projection_version,
            build_epoch: scope.build_epoch,
            node_count,
            edge_count,
            provenance_coverage_percent: Some(provenance_coverage_percent),
            last_error_message: None,
        },
    )
    .await?
    .ok_or_else(|| GraphServiceError::StateConflict {
        message: format!(
            "runtime graph rebuild snapshot conflict for library {} projection {} build {}",
            scope.library_id, scope.projection_version, scope.build_epoch
        ),
    })?;
    Ok(())
}

/// Dispatches a detached task that rebuilds the NDJSON topology under
/// the newly published topology generation key so
/// the next operator GET lands a cache hit. The projection pipeline
/// returns without waiting: prewarm failure is a soft degradation
/// (lazy rebuild still works), not a projection failure.
pub(crate) fn schedule_topology_prewarm(state: &AppState, library_id: Uuid) {
    let state = state.clone();
    tokio::spawn(async move {
        prewarm_graph_topology_cache(&state, library_id).await;
    });
}

async fn synchronize_projection_support_counts(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<()> {
    let outcome = repositories::synchronize_runtime_graph_support_and_prune(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to atomically synchronize active graph support")?;
    tracing::debug!(
        library_id = %scope.library_id,
        deleted_nodes = outcome.deleted_node_count,
        deleted_edges = outcome.deleted_edge_count,
        deleted_summaries = outcome.deleted_summary_count,
        "atomically synchronized active graph support",
    );

    Ok(())
}

async fn project_targeted_normalized_graph(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<GraphProjectionOutcome> {
    let mut targeted_edge_ids = scope.targeted_edge_ids.iter().copied().collect::<BTreeSet<_>>();
    let incident_edges = repositories::list_admitted_runtime_graph_edges_by_node_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &scope.targeted_node_ids,
    )
    .await
    .context("failed to load incident graph edges for targeted projection refresh")?;
    targeted_edge_ids.extend(incident_edges.iter().map(|edge| edge.id));
    let targeted_edge_ids = targeted_edge_ids.into_iter().collect::<Vec<_>>();
    let refreshed_edges = repositories::list_admitted_runtime_graph_edges_by_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &targeted_edge_ids,
    )
    .await
    .context("failed to load targeted graph edges for projection refresh")?;
    let support_node_ids = scope
        .targeted_node_ids
        .iter()
        .copied()
        .chain(refreshed_edges.iter().flat_map(|edge| [edge.from_node_id, edge.to_node_id]))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let refreshed_nodes = repositories::list_admitted_runtime_graph_nodes_by_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &support_node_ids,
    )
    .await
    .context("failed to load targeted graph nodes for projection refresh")?;

    // Same rationale as in `project_normalized_graph`: hand the
    // synchronous clone/sort/sanitize hot path to `spawn_blocking` so
    // the heartbeat loop keeps getting scheduled during reconcile.
    let projection_quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let targeted_sanitize_task = tokio::task::spawn_blocking(move || {
        projection_writes_from_rows(&projection_quality_guard, &refreshed_nodes, &refreshed_edges)
    });
    let mut projection_writes =
        targeted_sanitize_task.await.context("targeted graph projection sanitize task panicked")?;
    let projection_pruned_incident_edge_ids =
        repositories::list_runtime_graph_edge_ids_by_node_ids(
            &state.persistence.postgres,
            scope.library_id,
            scope.projection_version,
            &projection_writes.projection_pruned_node_ids,
        )
        .await
        .context("failed to load incident graph edges for targeted projection prune")?;
    projection_writes
        .projection_pruned_edge_ids
        .extend(projection_pruned_incident_edge_ids.iter().copied());
    projection_writes.projection_pruned_edge_ids.sort_unstable();
    projection_writes.projection_pruned_edge_ids.dedup();
    prune_disallowed_projection_rows(state, scope, &projection_writes).await?;

    let remove_node_ids = scope
        .targeted_node_ids
        .iter()
        .copied()
        .chain(projection_writes.remove_node_ids.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let remove_edge_ids = targeted_edge_ids
        .iter()
        .copied()
        .chain(projection_writes.remove_edge_ids.iter().copied())
        .chain(projection_pruned_incident_edge_ids)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let counts = repositories::count_admitted_runtime_graph_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to count admitted graph rows before targeted projection refresh")?;
    let node_count = usize::try_from(counts.node_count).unwrap_or_default();
    let edge_count = usize::try_from(counts.edge_count).unwrap_or_default();
    let provenance_coverage_percent =
        provenance_coverage_percent(state, scope.library_id, scope.projection_version).await?;
    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status: "building",
            node_count: i32::try_from(node_count).unwrap_or(i32::MAX),
            edge_count: i32::try_from(edge_count).unwrap_or(i32::MAX),
            provenance_coverage_percent: Some(provenance_coverage_percent),
            last_error_message: None,
        },
    )
    .await
    .context("failed to claim targeted graph snapshot build")?;

    if let Err(error) =
        execute_projection_write_with_guard(state, scope, "targeted_projection", || {
            state.graph_store.refresh_library_projection_targets(LibraryProjectionTargets {
                library_id: scope.library_id,
                projection_version: scope.projection_version,
                remove_node_ids: &remove_node_ids,
                remove_edge_ids: &remove_edge_ids,
                nodes: &projection_writes.nodes,
                edges: &projection_writes.edges,
            })
        })
        .await
    {
        let failure_message = error.to_string();
        persist_runtime_graph_snapshot(
            state,
            scope,
            GraphSnapshotState {
                graph_status: "failed",
                node_count: i32::try_from(node_count).unwrap_or(i32::MAX),
                edge_count: i32::try_from(edge_count).unwrap_or(i32::MAX),
                provenance_coverage_percent: Some(provenance_coverage_percent),
                last_error_message: Some(&failure_message),
            },
        )
        .await
        .context("failed to mark targeted graph snapshot as failed")?;
        return Err(error.context("failed to refresh targeted graph view"));
    }
    let graph_status =
        if node_count == 0 && edge_count == 0 { GRAPH_STATUS_EMPTY } else { GRAPH_STATUS_READY };

    persist_runtime_graph_snapshot(
        state,
        scope,
        GraphSnapshotState {
            graph_status,
            node_count: i32::try_from(node_count).unwrap_or(i32::MAX),
            edge_count: i32::try_from(edge_count).unwrap_or(i32::MAX),
            provenance_coverage_percent: Some(provenance_coverage_percent),
            last_error_message: None,
        },
    )
    .await
    .context("failed to persist targeted graph snapshot state")?;
    // Do not DEL the topology cache on targeted refreshes. The snapshot
    // upsert advances `topology_generation`, so new reads move to a new
    // `graph:{library_id}:v{projection_version}:g{generation}` key while
    // any in-flight reads on the previous key finish normally and old
    // bytes age out via TTL.
    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count,
        edge_count,
        graph_status: graph_status.to_string(),
    })
}

async fn execute_projection_write_with_guard<F, Fut>(
    state: &AppState,
    scope: &GraphProjectionScope,
    _scope_kind: &str,
    operation: F,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), GraphViewWriteError>>,
{
    let mut projection_lock = repositories::acquire_runtime_library_graph_lock(
        &state.persistence.postgres,
        scope.library_id,
    )
    .await
    .context("failed to acquire graph projection advisory lock")?;
    let build_is_current = repositories::runtime_graph_snapshot_build_is_current(
        &mut *projection_lock,
        scope.library_id,
        scope.projection_version,
        scope.build_epoch,
    )
    .await
    .context("failed to verify graph snapshot build ownership")?;
    let result = if build_is_current {
        execute_projection_write_retries(state, operation).await
    } else {
        Err(anyhow!(
            "graph snapshot publish conflict: build {} no longer owns library {} projection {}",
            scope.build_epoch,
            scope.library_id,
            scope.projection_version
        ))
    };
    let release_result =
        repositories::release_runtime_library_graph_lock(projection_lock, scope.library_id)
            .await
            .context("failed to release graph projection advisory lock");
    match (result, release_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(release_error)) => Err(release_error),
        (Err(error), Err(release_error)) => Err(release_error).context(error.to_string()),
    }
}

async fn execute_projection_write_retries<F, Fut>(
    state: &AppState,
    operation: F,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), GraphViewWriteError>>,
{
    let guard = &state.resolve_settle_blockers_services.graph_projection_guard;
    let mut contention_retries = 0usize;
    loop {
        match operation().await {
            Ok(()) => return Ok(()),
            Err(error) => match guard.classify_write_error(&error, contention_retries + 1) {
                GraphWriteFailureDecision::RetryContention => {
                    contention_retries += 1;
                    sleep(Duration::from_millis(200)).await;
                }
                GraphWriteFailureDecision::FailTerminal => {
                    return Err(anyhow!(error.to_string()));
                }
            },
        }
    }
}

async fn provenance_coverage_percent(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> Result<f64, GraphServiceError> {
    let (ready_document_count, projected_document_count) =
        content_document_list_repository::count_library_ready_and_projected_documents_in_snapshot(
            &state.persistence.postgres,
            library_id,
            projection_version,
        )
        .await
        .context("failed to count graph provenance coverage from one database snapshot")?;
    Ok(document_coverage_percent(projected_document_count, ready_document_count))
}

async fn provenance_coverage_percent_with_connection(
    connection: &mut PgConnection,
    library_id: Uuid,
    projection_version: i64,
) -> Result<f64, GraphServiceError> {
    let (ready_document_count, projected_document_count) =
        content_document_list_repository::count_library_ready_and_projected_documents_with_executor(
            connection,
            library_id,
            projection_version,
        )
        .await
        .context("failed to count rebuilt graph provenance coverage")?;
    Ok(document_coverage_percent(projected_document_count, ready_document_count))
}

fn document_coverage_percent(projected_document_count: i64, ready_document_count: i64) -> f64 {
    if ready_document_count <= 0 {
        return 0.0;
    }
    let projected_document_count = projected_document_count.clamp(0, ready_document_count);
    projected_document_count as f64 * 100.0 / ready_document_count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_coverage_reports_projected_document_share() {
        assert_eq!(document_coverage_percent(9, 100), 9.0);
        assert_eq!(document_coverage_percent(110, 100), 100.0);
        assert_eq!(document_coverage_percent(0, 0), 0.0);
    }

    #[test]
    fn rebuilt_graph_status_is_empty_only_when_both_counts_are_zero() {
        assert_eq!(rebuilt_graph_status(0, 0), GRAPH_STATUS_EMPTY);
        assert_eq!(rebuilt_graph_status(1, 0), GRAPH_STATUS_READY);
        assert_eq!(rebuilt_graph_status(0, 1), GRAPH_STATUS_READY);
        assert_eq!(rebuilt_graph_status(2, 3), GRAPH_STATUS_READY);
    }

    #[test]
    fn defaults_projection_version_to_one_when_snapshot_is_absent() {
        assert_eq!(active_projection_version(None), 1);
    }

    #[test]
    fn keeps_existing_projection_version_when_snapshot_exists() {
        let snapshot = RuntimeGraphSnapshotRow {
            library_id: Uuid::nil(),
            graph_status: "ready".to_string(),
            projection_version: 7,
            topology_generation: 1,
            node_count: 3,
            edge_count: 2,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(active_projection_version(Some(&snapshot)), 7);
    }

    #[test]
    fn falls_back_to_one_when_snapshot_version_is_zero() {
        let snapshot = RuntimeGraphSnapshotRow {
            library_id: Uuid::nil(),
            graph_status: "building".to_string(),
            projection_version: 0,
            topology_generation: 0,
            node_count: 0,
            edge_count: 0,
            provenance_coverage_percent: Some(0.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(active_projection_version(Some(&snapshot)), 1);
    }

    #[test]
    fn increments_projection_version_for_rebuilds() {
        let snapshot = RuntimeGraphSnapshotRow {
            library_id: Uuid::nil(),
            graph_status: "ready".to_string(),
            projection_version: 3,
            topology_generation: 1,
            node_count: 2,
            edge_count: 1,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(next_projection_version(Some(&snapshot)), 4);
        assert_eq!(next_projection_version(None), 1);
    }

    #[test]
    fn projection_scope_defers_source_truth_for_lifecycle_owned_publication() {
        let immediate = GraphProjectionScope::new(Uuid::nil(), 4);
        assert_eq!(immediate.source_truth_publication, GraphSourceTruthPublication::Immediate);
        let scope = immediate.defer_source_truth_to_lifecycle();

        assert_eq!(
            scope.source_truth_publication,
            GraphSourceTruthPublication::DeferredToLifecycle
        );
    }

    #[test]
    fn projection_writes_apply_quality_guard_before_graph_store_write() {
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let literal_id = Uuid::now_v7();
        let quality_guard = GraphQualityGuardService::new(true, true);
        let nodes = vec![
            runtime_node(library_id, document_id, "document:alpha", "Alpha Doc", "document"),
            runtime_node(library_id, entity_id, "entity:alpha", "Alpha Entity", "entity"),
            runtime_node(library_id, literal_id, "attribute:false", "false", "attribute"),
        ];
        let edges = vec![
            runtime_edge(
                library_id,
                document_id,
                entity_id,
                "document:alpha--mentions--entity:alpha",
            ),
            runtime_edge(
                library_id,
                document_id,
                literal_id,
                "document:alpha--mentions--attribute:false",
            ),
        ];

        let projection_writes = projection_writes_from_rows(&quality_guard, &nodes, &edges);

        assert_eq!(projection_writes.nodes.len(), 2);
        assert!(projection_writes.nodes.iter().all(|node| node.node_id != literal_id));
        assert_eq!(projection_writes.edges.len(), 1);
        assert_eq!(projection_writes.edges[0].to_node_id, entity_id);
        assert_eq!(projection_writes.remove_node_ids, vec![literal_id]);
        assert_eq!(projection_writes.remove_edge_ids.len(), 1);
        assert_eq!(projection_writes.projection_pruned_node_ids, vec![literal_id]);
        assert_eq!(projection_writes.projection_pruned_edge_ids.len(), 1);
    }

    #[test]
    fn projection_writes_do_not_prune_view_only_orphans() {
        let library_id = Uuid::now_v7();
        let orphan_id = Uuid::now_v7();
        let quality_guard = GraphQualityGuardService::new(true, true);
        let nodes = vec![runtime_node(library_id, orphan_id, "entity:orphan", "Orphan", "entity")];

        let projection_writes = projection_writes_from_rows(&quality_guard, &nodes, &[]);

        assert!(projection_writes.nodes.is_empty());
        assert_eq!(projection_writes.remove_node_ids, vec![orphan_id]);
        assert!(projection_writes.projection_pruned_node_ids.is_empty());
        assert!(projection_writes.projection_pruned_edge_ids.is_empty());
    }

    #[test]
    fn projection_writes_track_relation_rejections_for_full_rebuild_prune() {
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let quality_guard = GraphQualityGuardService::new(true, true);
        let nodes = vec![
            runtime_node(library_id, document_id, "document:alpha", "Alpha Doc", "document"),
            runtime_node(library_id, entity_id, "entity:alpha", "Alpha Entity", "entity"),
        ];
        let mut rejected_edge = runtime_edge(
            library_id,
            document_id,
            entity_id,
            "document:alpha--invalid--entity:alpha",
        );
        rejected_edge.relation_type = "Invalid Predicate".to_string();
        let rejected_edge_id = rejected_edge.id;

        let projection_writes =
            projection_writes_from_rows(&quality_guard, &nodes, &[rejected_edge]);

        assert_eq!(projection_writes.nodes.len(), 1);
        assert_eq!(projection_writes.nodes[0].node_id, document_id);
        assert!(projection_writes.edges.is_empty());
        assert_eq!(projection_writes.remove_node_ids, vec![entity_id]);
        assert_eq!(projection_writes.remove_edge_ids, vec![rejected_edge_id]);
        assert!(projection_writes.projection_pruned_node_ids.is_empty());
        assert!(projection_writes.projection_pruned_edge_ids.is_empty());
    }

    fn runtime_node(
        library_id: Uuid,
        id: Uuid,
        identity_key: &str,
        label: &str,
        node_type: &str,
    ) -> repositories::RuntimeGraphNodeRow {
        repositories::RuntimeGraphNodeRow {
            id,
            library_id,
            identity_key: identity_key.to_string(),
            label: label.to_string(),
            node_type: node_type.to_string(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn runtime_edge(
        library_id: Uuid,
        from_node_id: Uuid,
        to_node_id: Uuid,
        identity_key: &str,
    ) -> repositories::RuntimeGraphEdgeRow {
        repositories::RuntimeGraphEdgeRow {
            id: Uuid::now_v7(),
            library_id,
            from_node_id,
            to_node_id,
            relation_type: "mentions".to_string(),
            identity_key: identity_key.to_string(),
            summary: None,
            weight: None,
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}
