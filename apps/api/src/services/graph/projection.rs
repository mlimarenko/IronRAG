use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, anyhow};
use tokio::time::sleep;
use uuid::Uuid;

use crate::domains::ops::{GRAPH_STATUS_EMPTY, GRAPH_STATUS_READY};
use crate::services::graph::projection_guard::GraphWriteFailureDecision;
use crate::{
    app::state::AppState,
    infra::{
        knowledge_rows::{
            GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
            sanitize_graph_view_writes,
        },
        repositories::{self, RuntimeGraphSnapshotRow},
    },
    services::graph::{
        error::GraphServiceError, quality_guard::GraphQualityGuardService,
        summary::GraphSummaryRefreshRequest,
    },
    services::knowledge::graph_stream::prewarm_graph_topology_cache,
    shared::json_coercion::from_value_or_default,
};

const INLINE_SUMMARY_REFRESH_TARGET_LIMIT: usize = 500;

#[derive(Debug, Clone)]
pub struct GraphProjectionScope {
    pub library_id: Uuid,
    pub projection_version: i64,
    pub targeted_node_ids: Vec<Uuid>,
    pub targeted_edge_ids: Vec<Uuid>,
    pub summary_refresh: Option<GraphSummaryRefreshRequest>,
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
    canonical_pruned_node_ids: Vec<Uuid>,
    canonical_pruned_edge_ids: Vec<Uuid>,
}

impl GraphProjectionOutcome {
    #[must_use]
    pub fn has_materialized_graph(&self) -> bool {
        self.graph_status == GRAPH_STATUS_READY && (self.node_count > 0 || self.edge_count > 0)
    }
}

impl GraphProjectionScope {
    #[must_use]
    pub const fn new(library_id: Uuid, projection_version: i64) -> Self {
        Self {
            library_id,
            projection_version,
            targeted_node_ids: Vec::new(),
            targeted_edge_ids: Vec::new(),
            summary_refresh: None,
        }
    }

    #[must_use]
    pub fn with_summary_refresh(mut self, summary_refresh: GraphSummaryRefreshRequest) -> Self {
        self.summary_refresh = Some(summary_refresh);
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
    pub fn is_targeted_refresh(&self) -> bool {
        !self.targeted_node_ids.is_empty() || !self.targeted_edge_ids.is_empty()
    }
}

#[must_use]
pub fn active_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1)
}

#[must_use]
pub fn next_projection_version(snapshot: Option<&RuntimeGraphSnapshotRow>) -> i64 {
    snapshot.map(|_| active_projection_version(snapshot) + 1).unwrap_or(1)
}

pub async fn resolve_projection_scope(
    state: &AppState,
    library_id: Uuid,
) -> Result<GraphProjectionScope, GraphServiceError> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while resolving projection scope")?;
    Ok(GraphProjectionScope::new(library_id, active_projection_version(snapshot.as_ref())))
}

pub async fn ensure_empty_graph_snapshot(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        library_id,
        "empty",
        projection_version,
        0,
        0,
        Some(0.0),
        None,
    )
    .await
    .context("failed to persist empty graph snapshot")?;

    Ok(GraphProjectionOutcome {
        projection_version,
        node_count: 0,
        edge_count: 0,
        graph_status: "empty".to_string(),
    })
}

pub async fn project_canonical_graph(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    // `synchronize_projection_support_counts` runs three library-wide
    // sweeps (recalculate support counts, prune zero-support edges,
    // prune zero-support nodes). On a mid-sized graph (27k edges / 10k
    // nodes / 37k canonical summaries) that sweep costs ~5s. Running
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
        return project_targeted_canonical_graph(state, scope).await.map_err(Into::into);
    }
    synchronize_projection_support_counts(state, scope).await?;
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load canonical graph nodes for projection")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load canonical graph edges for projection")?;

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        "building",
        scope.projection_version,
        i32::try_from(nodes.len()).unwrap_or(i32::MAX),
        i32::try_from(edges.len()).unwrap_or(i32::MAX),
        Some(provenance_coverage_percent(&nodes, &edges)),
        None,
    )
    .await
    .context("failed to mark graph snapshot as building")?;
    // Intentionally DO NOT invalidate the topology cache here. The
    // `building` transition advertises work in flight but the previous
    // `ready` snapshot (same projection_version as the cache key) is
    // still the canonical active graph until the next `ready` upsert
    // below swaps it out. Dropping the cache here caused every ingest
    // cycle on a library to DEL the only live key before the replacement
    // landed, forcing every concurrent GET to rebuild from Postgres and
    // producing 25 s cold-path storms on reference libraries.

    if nodes.is_empty() && edges.is_empty() {
        let outcome =
            ensure_empty_graph_snapshot(state, scope.library_id, scope.projection_version).await?;
        maybe_apply_summary_refresh(state, scope).await?;
        return Ok(outcome);
    }

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
        repositories::upsert_runtime_graph_snapshot(
            &state.persistence.postgres,
            scope.library_id,
            "failed",
            scope.projection_version,
            i32::try_from(projection_writes.nodes.len()).unwrap_or(i32::MAX),
            i32::try_from(projection_writes.edges.len()).unwrap_or(i32::MAX),
            Some(provenance_coverage_percent_from_counts(
                projection_writes.nodes.len(),
                projection_writes.edges.len(),
            )),
            Some(&failure_message),
        )
        .await
        .context("failed to mark graph snapshot as failed after graph-store refresh error")?;
        return Err(error.context("failed to refresh the canonical graph view").into());
    }

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        "ready",
        scope.projection_version,
        i32::try_from(projection_writes.nodes.len()).unwrap_or(i32::MAX),
        i32::try_from(projection_writes.edges.len()).unwrap_or(i32::MAX),
        Some(provenance_coverage_percent_from_counts(
            projection_writes.nodes.len(),
            projection_writes.edges.len(),
        )),
        None,
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
    maybe_apply_summary_refresh(state, scope).await?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: projection_writes.nodes.len(),
        edge_count: projection_writes.edges.len(),
        graph_status: "ready".to_string(),
    })
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
            canonical_key: node.canonical_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: from_value_or_default("runtime_graph_node.aliases_json", &node.aliases_json),
            metadata_json: node.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let input_node_ids = node_writes.iter().map(|node| node.node_id).collect::<BTreeSet<_>>();
    let canonical_pruned_node_ids = node_writes
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
            canonical_key: edge.canonical_key.clone(),
            support_count: edge.support_count,
            summary: edge.summary.clone(),
            weight: edge.weight,
            metadata_json: edge.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let input_edge_ids = edge_writes.iter().map(|edge| edge.edge_id).collect::<BTreeSet<_>>();
    let canonical_pruned_edge_ids = edge_writes
        .iter()
        .filter(|edge| {
            canonical_pruned_node_ids.contains(&edge.from_node_id)
                || canonical_pruned_node_ids.contains(&edge.to_node_id)
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
        canonical_pruned_node_ids: canonical_pruned_node_ids.into_iter().collect(),
        canonical_pruned_edge_ids,
    }
}

async fn prune_disallowed_projection_rows(
    state: &AppState,
    scope: &GraphProjectionScope,
    projection_writes: &ProjectionWrites,
) -> Result<(), GraphServiceError> {
    if projection_writes.canonical_pruned_node_ids.is_empty()
        && projection_writes.canonical_pruned_edge_ids.is_empty()
    {
        return Ok(());
    }

    repositories::delete_runtime_graph_edges_by_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &projection_writes.canonical_pruned_edge_ids,
    )
    .await
    .context("failed to prune disallowed graph projection edges")?;
    repositories::delete_runtime_graph_nodes_by_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &projection_writes.canonical_pruned_node_ids,
    )
    .await
    .context("failed to prune disallowed graph projection nodes")?;
    repositories::delete_runtime_graph_canonical_summaries_for_orphan_targets(
        &state.persistence.postgres,
        scope.library_id,
        &projection_writes.canonical_pruned_node_ids,
        &projection_writes.canonical_pruned_edge_ids,
    )
    .await
    .context("failed to prune disallowed graph projection summaries")?;
    Ok(())
}

/// Dispatches a detached task that rebuilds the NDJSON topology under
/// the newly published topology generation key so
/// the next operator GET lands a cache hit. The projection pipeline
/// returns without waiting: prewarm failure is a soft degradation
/// (lazy rebuild still works), not a projection failure.
fn schedule_topology_prewarm(state: &AppState, library_id: Uuid) {
    let state = state.clone();
    tokio::spawn(async move {
        prewarm_graph_topology_cache(&state, library_id).await;
    });
}

async fn synchronize_projection_support_counts(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<()> {
    repositories::recalculate_runtime_graph_support_counts(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to recalculate canonical graph support counts before projection")?;

    let deleted_edge_keys = repositories::delete_runtime_graph_edges_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph edges before projection")?;

    let deleted_node_keys = repositories::delete_runtime_graph_nodes_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph nodes before projection")?;

    if !deleted_node_keys.is_empty() {
        let deleted = state
            .graph_store
            .delete_entities_by_canonical_keys(scope.library_id, &deleted_node_keys)
            .await
            .unwrap_or(0);
        tracing::info!(
            library_id = %scope.library_id,
            pg_deleted = deleted_node_keys.len(),
            deleted = deleted,
            "synced orphaned entity deletions to PostgreSQL"
        );
    }

    if !deleted_edge_keys.is_empty() {
        let deleted = state
            .graph_store
            .delete_relations_by_canonical_keys(scope.library_id, &deleted_edge_keys)
            .await
            .unwrap_or(0);
        tracing::info!(
            library_id = %scope.library_id,
            pg_deleted = deleted_edge_keys.len(),
            deleted = deleted,
            "synced orphaned relation deletions to PostgreSQL"
        );
    }

    Ok(())
}

async fn project_targeted_canonical_graph(
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

    // Same rationale as in `project_canonical_graph`: hand the
    // synchronous clone/sort/sanitize hot path to `spawn_blocking` so
    // the heartbeat loop keeps getting scheduled during reconcile.
    let projection_quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let targeted_sanitize_task = tokio::task::spawn_blocking(move || {
        projection_writes_from_rows(&projection_quality_guard, &refreshed_nodes, &refreshed_edges)
    });
    let mut projection_writes =
        targeted_sanitize_task.await.context("targeted graph projection sanitize task panicked")?;
    let canonical_pruned_incident_edge_ids = repositories::list_runtime_graph_edge_ids_by_node_ids(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
        &projection_writes.canonical_pruned_node_ids,
    )
    .await
    .context("failed to load incident graph edges for targeted projection prune")?;
    projection_writes
        .canonical_pruned_edge_ids
        .extend(canonical_pruned_incident_edge_ids.iter().copied());
    projection_writes.canonical_pruned_edge_ids.sort_unstable();
    projection_writes.canonical_pruned_edge_ids.dedup();
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
        .chain(canonical_pruned_incident_edge_ids)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    execute_projection_write_with_guard(state, scope, "targeted_projection", || {
        state.graph_store.refresh_library_projection_targets(
            scope.library_id,
            scope.projection_version,
            &remove_node_ids,
            &remove_edge_ids,
            &projection_writes.nodes,
            &projection_writes.edges,
        )
    })
    .await
    .context("failed to refresh targeted graph view")?;

    let counts = repositories::count_admitted_runtime_graph_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to count admitted graph rows after targeted projection refresh")?;
    let node_count = usize::try_from(counts.node_count).unwrap_or_default();
    let edge_count = usize::try_from(counts.edge_count).unwrap_or_default();
    let graph_status =
        if node_count == 0 && edge_count == 0 { GRAPH_STATUS_EMPTY } else { GRAPH_STATUS_READY };

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        graph_status,
        scope.projection_version,
        i32::try_from(node_count).unwrap_or(i32::MAX),
        i32::try_from(edge_count).unwrap_or(i32::MAX),
        Some(if node_count == 0 && edge_count == 0 { 0.0 } else { 100.0 }),
        None,
    )
    .await
    .context("failed to persist targeted graph snapshot state")?;
    // Do not DEL the topology cache on targeted refreshes. The snapshot
    // upsert advances `topology_generation`, so new reads move to a new
    // `graph:{library_id}:v{projection_version}:g{generation}` key while
    // any in-flight reads on the previous key finish normally and old
    // bytes age out via TTL.
    maybe_apply_summary_refresh(state, scope).await?;

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
    let guard = &state.resolve_settle_blockers_services.graph_projection_guard;
    let projection_lock = repositories::acquire_runtime_library_graph_lock(
        &state.persistence.postgres,
        scope.library_id,
    )
    .await
    .context("failed to acquire graph projection advisory lock")?;
    let result = async {
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
    .await;
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

async fn maybe_apply_summary_refresh(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<()> {
    let Some(summary_refresh) = scope.summary_refresh.as_ref() else {
        return Ok(());
    };
    if !summary_refresh.is_active() {
        return Ok(());
    }
    state
        .retrieval_intelligence_services
        .graph_summary
        .invalidate_summaries(state, scope.library_id, summary_refresh)
        .await
        .context("failed to refresh canonical summaries after graph projection")?;
    let affected_targets = inline_summary_refresh_target_count(state, scope, summary_refresh)
        .await
        .context("failed to count affected canonical summary targets after graph projection")?;
    if !state
        .retrieval_intelligence_services
        .graph_summary
        .should_batch_refresh(affected_targets, INLINE_SUMMARY_REFRESH_TARGET_LIMIT)
    {
        tracing::info!(
            library_id = %scope.library_id,
            affected_targets,
            inline_limit = INLINE_SUMMARY_REFRESH_TARGET_LIMIT,
            targeted = summary_refresh.is_targeted(),
            broad = summary_refresh.broad_refresh,
            "skipping inline canonical summary refresh for large graph mutation",
        );
        return Ok(());
    }
    state
        .retrieval_intelligence_services
        .graph_summary
        .refresh_summaries(state, scope.library_id, summary_refresh)
        .await
        .context("failed to generate canonical summaries after graph projection")?;
    Ok(())
}

async fn inline_summary_refresh_target_count(
    state: &AppState,
    scope: &GraphProjectionScope,
    summary_refresh: &GraphSummaryRefreshRequest,
) -> anyhow::Result<usize> {
    if summary_refresh.is_targeted() {
        return Ok(summary_refresh.node_ids.len().saturating_add(summary_refresh.edge_ids.len()));
    }
    if !summary_refresh.broad_refresh {
        return Ok(0);
    }
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, scope.library_id)
            .await
            .context("failed to load runtime graph snapshot for summary refresh sizing")?;
    Ok(summary_target_count_from_snapshot(snapshot.as_ref()))
}

fn summary_target_count_from_snapshot(snapshot: Option<&RuntimeGraphSnapshotRow>) -> usize {
    let Some(snapshot) = snapshot else {
        return 0;
    };
    let nodes = usize::try_from(snapshot.node_count.max(0)).unwrap_or_default();
    let edges = usize::try_from(snapshot.edge_count.max(0)).unwrap_or_default();
    nodes.saturating_add(edges)
}

fn provenance_coverage_percent(
    nodes: &[repositories::RuntimeGraphNodeRow],
    edges: &[repositories::RuntimeGraphEdgeRow],
) -> f64 {
    provenance_coverage_percent_from_counts(nodes.len(), edges.len())
}

fn provenance_coverage_percent_from_counts(node_count: usize, edge_count: usize) -> f64 {
    if node_count == 0 && edge_count == 0 { 0.0 } else { 100.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn projection_scope_can_carry_summary_refresh_requests() {
        let scope = GraphProjectionScope::new(Uuid::nil(), 4).with_summary_refresh(
            GraphSummaryRefreshRequest::targeted(vec![Uuid::nil()], Vec::new())
                .with_source_truth_version(11),
        );

        assert_eq!(
            scope.summary_refresh.as_ref().and_then(|refresh| refresh.source_truth_version),
            Some(11)
        );
        assert!(
            scope.summary_refresh.as_ref().is_some_and(GraphSummaryRefreshRequest::is_targeted)
        );
    }

    #[test]
    fn counts_summary_targets_from_snapshot_without_underflow() {
        let snapshot = RuntimeGraphSnapshotRow {
            library_id: Uuid::nil(),
            graph_status: "ready".to_string(),
            projection_version: 1,
            topology_generation: 1,
            node_count: -1,
            edge_count: 8,
            provenance_coverage_percent: Some(100.0),
            last_built_at: None,
            last_error_message: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(summary_target_count_from_snapshot(Some(&snapshot)), 8);
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
        assert_eq!(projection_writes.canonical_pruned_node_ids, vec![literal_id]);
        assert_eq!(projection_writes.canonical_pruned_edge_ids.len(), 1);
    }

    #[test]
    fn projection_writes_do_not_canonically_prune_view_only_orphans() {
        let library_id = Uuid::now_v7();
        let orphan_id = Uuid::now_v7();
        let quality_guard = GraphQualityGuardService::new(true, true);
        let nodes = vec![runtime_node(library_id, orphan_id, "entity:orphan", "Orphan", "entity")];

        let projection_writes = projection_writes_from_rows(&quality_guard, &nodes, &[]);

        assert!(projection_writes.nodes.is_empty());
        assert_eq!(projection_writes.remove_node_ids, vec![orphan_id]);
        assert!(projection_writes.canonical_pruned_node_ids.is_empty());
        assert!(projection_writes.canonical_pruned_edge_ids.is_empty());
    }

    fn runtime_node(
        library_id: Uuid,
        id: Uuid,
        canonical_key: &str,
        label: &str,
        node_type: &str,
    ) -> repositories::RuntimeGraphNodeRow {
        repositories::RuntimeGraphNodeRow {
            id,
            library_id,
            canonical_key: canonical_key.to_string(),
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
        canonical_key: &str,
    ) -> repositories::RuntimeGraphEdgeRow {
        repositories::RuntimeGraphEdgeRow {
            id: Uuid::now_v7(),
            library_id,
            from_node_id,
            to_node_id,
            relation_type: "mentions".to_string(),
            canonical_key: canonical_key.to_string(),
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
