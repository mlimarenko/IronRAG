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
        arangodb::graph_store::{
            GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError, sanitize_graph_view_writes,
        },
        repositories::{self, RuntimeGraphSnapshotRow},
    },
    services::graph::summary::GraphSummaryRefreshRequest,
    services::knowledge::graph_stream::invalidate_graph_topology_cache,
    shared::json_coercion::from_value_or_default,
};

/// Best-effort wipe of the compact graph topology cache for one library.
/// Called after every `upsert_runtime_graph_snapshot` so the next topology
/// request rebuilds against the freshly admitted projection. Cache failures
/// must not block the projection pipeline — log and continue.
async fn invalidate_topology_cache(state: &AppState, library_id: Uuid) {
    if let Err(error) = invalidate_graph_topology_cache(&state.persistence.redis, library_id).await
    {
        tracing::warn!(
            %library_id,
            error = format!("{error:#}"),
            "graph topology cache invalidation failed",
        );
    }
}

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
) -> anyhow::Result<GraphProjectionScope> {
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
) -> anyhow::Result<GraphProjectionOutcome> {
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
    invalidate_topology_cache(state, library_id).await;

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
) -> anyhow::Result<GraphProjectionOutcome> {
    // `synchronize_projection_support_counts` runs three library-wide
    // sweeps (recalculate support counts, prune zero-support edges,
    // prune zero-support nodes). On a mid-sized graph (27k edges / 10k
    // nodes / 37k canonical summaries) that sweep costs ~5s. Running
    // it after **every** chunk merge is O(chunks × graph-size) and
    // dominates worker throughput — a 147-job queue that used to drain
    // in ~15s per doc stretches to >10 min because each chunk redoes
    // the full-library walk.
    //
    // For a targeted refresh (one chunk's contribution) the incident
    // nodes/edges we just upserted cannot create new zero-support
    // orphans anywhere OUTSIDE their own subgraph — support counts
    // only drop when a source_truth_version is superseded, and that
    // happens during full projections. So the sync is safe to skip on
    // targeted paths and run only on full rebuilds.
    if scope.is_targeted_refresh() {
        return project_targeted_canonical_graph(state, scope).await;
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
    invalidate_topology_cache(state, scope.library_id).await;

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
    let sanitize_task = tokio::task::spawn_blocking(move || {
        let node_writes = nodes
            .iter()
            .map(|node| GraphViewNodeWrite {
                node_id: node.id,
                canonical_key: node.canonical_key.clone(),
                label: node.label.clone(),
                node_type: node.node_type.clone(),
                support_count: node.support_count,
                summary: node.summary.clone(),
                aliases: from_value_or_default(
                    "runtime_graph_node.aliases_json",
                    &node.aliases_json,
                ),
                metadata_json: node.metadata_json.clone(),
            })
            .collect::<Vec<_>>();
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
        let (node_writes, edge_writes, _skipped_edge_count) =
            sanitize_graph_view_writes(&node_writes, &edge_writes);
        (nodes, edges, node_writes, edge_writes)
    });
    let (nodes, edges, node_writes, edge_writes) =
        sanitize_task.await.context("graph projection sanitize task panicked")?;

    if let Err(error) =
        execute_projection_write_with_guard(state, scope, "library_projection", || {
            state.arango_graph_store.replace_library_projection(
                scope.library_id,
                scope.projection_version,
                &node_writes,
                &edge_writes,
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
            i32::try_from(nodes.len()).unwrap_or(i32::MAX),
            i32::try_from(edges.len()).unwrap_or(i32::MAX),
            Some(provenance_coverage_percent(&nodes, &edges)),
            Some(&failure_message),
        )
        .await
        .context("failed to mark graph snapshot as failed after graph-store refresh error")?;
        invalidate_topology_cache(state, scope.library_id).await;
        return Err(error).context("failed to refresh the canonical graph view");
    }

    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        scope.library_id,
        "ready",
        scope.projection_version,
        i32::try_from(nodes.len()).unwrap_or(i32::MAX),
        i32::try_from(edges.len()).unwrap_or(i32::MAX),
        Some(provenance_coverage_percent(&nodes, &edges)),
        None,
    )
    .await
    .context("failed to mark graph snapshot as ready")?;
    invalidate_topology_cache(state, scope.library_id).await;
    maybe_apply_summary_refresh(state, scope).await?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: node_writes.len(),
        edge_count: edge_writes.len(),
        graph_status: "ready".to_string(),
    })
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
        let arango_deleted = state
            .arango_graph_store
            .delete_entities_by_canonical_keys(scope.library_id, &deleted_node_keys)
            .await
            .unwrap_or(0);
        tracing::info!(
            library_id = %scope.library_id,
            pg_deleted = deleted_node_keys.len(),
            arango_deleted = arango_deleted,
            "synced orphaned entity deletions to ArangoDB"
        );
    }

    if !deleted_edge_keys.is_empty() {
        let arango_deleted = state
            .arango_graph_store
            .delete_relations_by_canonical_keys(scope.library_id, &deleted_edge_keys)
            .await
            .unwrap_or(0);
        tracing::info!(
            library_id = %scope.library_id,
            pg_deleted = deleted_edge_keys.len(),
            arango_deleted = arango_deleted,
            "synced orphaned relation deletions to ArangoDB"
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
    let targeted_sanitize_task = tokio::task::spawn_blocking(move || {
        let node_writes = refreshed_nodes
            .iter()
            .map(|node| GraphViewNodeWrite {
                node_id: node.id,
                canonical_key: node.canonical_key.clone(),
                label: node.label.clone(),
                node_type: node.node_type.clone(),
                support_count: node.support_count,
                summary: node.summary.clone(),
                aliases: from_value_or_default(
                    "runtime_graph_node.aliases_json",
                    &node.aliases_json,
                ),
                metadata_json: node.metadata_json.clone(),
            })
            .collect::<Vec<_>>();
        let edge_writes = refreshed_edges
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
        let (node_writes, edge_writes, _skipped_edge_count) =
            sanitize_graph_view_writes(&node_writes, &edge_writes);
        (node_writes, edge_writes)
    });
    let (node_writes, edge_writes) =
        targeted_sanitize_task.await.context("targeted graph projection sanitize task panicked")?;

    execute_projection_write_with_guard(state, scope, "targeted_projection", || {
        state.arango_graph_store.refresh_library_projection_targets(
            scope.library_id,
            scope.projection_version,
            &scope.targeted_node_ids,
            &targeted_edge_ids,
            &node_writes,
            &edge_writes,
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
    invalidate_topology_cache(state, scope.library_id).await;
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
    state
        .retrieval_intelligence_services
        .graph_summary
        .refresh_summaries(state, scope.library_id, summary_refresh)
        .await
        .context("failed to generate canonical summaries after graph projection")?;
    Ok(())
}

fn provenance_coverage_percent(
    nodes: &[repositories::RuntimeGraphNodeRow],
    edges: &[repositories::RuntimeGraphEdgeRow],
) -> f64 {
    if nodes.is_empty() && edges.is_empty() { 0.0 } else { 100.0 }
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
}
