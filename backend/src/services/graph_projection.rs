use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::Utc;
use tokio::time::sleep;
use uuid::Uuid;

use crate::services::graph_projection_guard::GraphProjectionFailureDecision;
use crate::{
    app::state::AppState,
    infra::{
        graph_store::{
            GraphProjectionEdgeWrite, GraphProjectionNodeWrite, GraphProjectionWriteError,
            sanitize_projection_writes,
        },
        repositories::{self, RuntimeGraphSnapshotRow},
    },
    services::graph_summary::GraphSummaryRefreshRequest,
};

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
        targeted_node_ids: Vec<Uuid>,
        targeted_edge_ids: Vec<Uuid>,
    ) -> Self {
        self.targeted_node_ids =
            targeted_node_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        self.targeted_edge_ids =
            targeted_edge_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
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

    Ok(GraphProjectionOutcome {
        projection_version,
        node_count: 0,
        edge_count: 0,
        graph_status: "empty".to_string(),
    })
}

pub async fn mark_graph_snapshot_stale(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    node_count: usize,
    edge_count: usize,
    reason: Option<&str>,
) -> anyhow::Result<GraphProjectionOutcome> {
    repositories::upsert_runtime_graph_snapshot(
        &state.persistence.postgres,
        library_id,
        "stale",
        projection_version,
        i32::try_from(node_count).unwrap_or(i32::MAX),
        i32::try_from(edge_count).unwrap_or(i32::MAX),
        Some(if node_count == 0 && edge_count == 0 { 0.0 } else { 100.0 }),
        reason,
    )
    .await
    .context("failed to mark graph snapshot as stale")?;

    Ok(GraphProjectionOutcome {
        projection_version,
        node_count,
        edge_count,
        graph_status: "stale".to_string(),
    })
}

pub async fn project_canonical_graph(
    state: &AppState,
    scope: &GraphProjectionScope,
) -> anyhow::Result<GraphProjectionOutcome> {
    synchronize_projection_support_counts(state, scope).await?;
    if scope.is_targeted_refresh() {
        return project_targeted_canonical_graph(state, scope).await;
    }
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to load canonical graph nodes for projection")?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_projection(
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

    if nodes.is_empty() && edges.is_empty() {
        let outcome =
            ensure_empty_graph_snapshot(state, scope.library_id, scope.projection_version).await?;
        maybe_apply_summary_refresh(state, scope).await?;
        return Ok(outcome);
    }

    let node_writes = nodes
        .iter()
        .map(|node| GraphProjectionNodeWrite {
            node_id: node.id,
            canonical_key: node.canonical_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: serde_json::from_value(node.aliases_json.clone()).unwrap_or_default(),
            metadata_json: node.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let edge_writes = edges
        .iter()
        .map(|edge| GraphProjectionEdgeWrite {
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
        sanitize_projection_writes(&node_writes, &edge_writes);

    if let Err(error) = execute_projection_write_with_guard(
        state,
        scope,
        "library_projection",
        node_writes.len(),
        edge_writes.len(),
        || {
            state.graph_store.replace_library_projection(
                scope.library_id,
                scope.projection_version,
                &node_writes,
                &edge_writes,
            )
        },
    )
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
        .context("failed to mark graph snapshot as failed after Neo4j projection error")?;
        return Err(error).context("failed to project canonical graph into Neo4j");
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
    maybe_apply_summary_refresh(state, scope).await?;

    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: node_writes.len(),
        edge_count: edge_writes.len(),
        graph_status: "ready".to_string(),
    })
}

pub async fn rebuild_projection_from_canonical(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> anyhow::Result<GraphProjectionOutcome> {
    project_canonical_graph(state, &GraphProjectionScope::new(library_id, projection_version)).await
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
    repositories::delete_runtime_graph_edges_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph edges before projection")?;
    repositories::delete_runtime_graph_nodes_without_support(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to prune zero-support graph nodes before projection")?;

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

    let node_writes = refreshed_nodes
        .iter()
        .map(|node| GraphProjectionNodeWrite {
            node_id: node.id,
            canonical_key: node.canonical_key.clone(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            support_count: node.support_count,
            summary: node.summary.clone(),
            aliases: serde_json::from_value(node.aliases_json.clone()).unwrap_or_default(),
            metadata_json: node.metadata_json.clone(),
        })
        .collect::<Vec<_>>();
    let edge_writes = refreshed_edges
        .iter()
        .map(|edge| GraphProjectionEdgeWrite {
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
        sanitize_projection_writes(&node_writes, &edge_writes);

    execute_projection_write_with_guard(
        state,
        scope,
        "targeted_projection",
        node_writes.len(),
        edge_writes.len(),
        || {
            state.graph_store.refresh_library_projection_targets(
                scope.library_id,
                scope.projection_version,
                &scope.targeted_node_ids,
                &targeted_edge_ids,
                &node_writes,
                &edge_writes,
            )
        },
    )
    .await
    .context("failed to refresh targeted graph projection in Neo4j")?;

    let counts = repositories::count_admitted_runtime_graph_projection(
        &state.persistence.postgres,
        scope.library_id,
        scope.projection_version,
    )
    .await
    .context("failed to count admitted graph rows after targeted projection refresh")?;
    let node_count = usize::try_from(counts.node_count).unwrap_or_default();
    let edge_count = usize::try_from(counts.edge_count).unwrap_or_default();
    let graph_status = if node_count == 0 && edge_count == 0 { "empty" } else { "ready" };

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
    scope_kind: &str,
    pending_node_write_count: usize,
    pending_edge_write_count: usize,
    operation: F,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), GraphProjectionWriteError>>,
{
    let guard = &state.resolve_settle_blockers_services.graph_projection_guard;
    let mut scope_row = repositories::create_runtime_graph_projection_scope(
        &state.persistence.postgres,
        &repositories::RuntimeGraphProjectionScopeInput {
            id: Uuid::now_v7(),
            project_id: scope.library_id,
            scope_kind: scope_kind.to_string(),
            attempt_no: 1,
            lock_state: crate::domains::runtime_graph::RuntimeGraphProjectionLockState::Idle,
            write_state: crate::domains::runtime_graph::RuntimeGraphProjectionWriteState::Pending,
            deadlock_retry_count: 0,
            failure_kind: None,
            started_at: Utc::now(),
            finished_at: None,
        },
    )
    .await
    .context("failed to create graph projection scope row")?;
    persist_graph_diagnostics_snapshot(
        state,
        scope.library_id,
        pending_node_write_count,
        pending_edge_write_count,
        None,
        None,
        true,
    )
    .await?;

    let projection_lock = repositories::acquire_runtime_library_projection_lock(
        &state.persistence.postgres,
        scope.library_id,
    )
    .await
    .context("failed to acquire graph projection advisory lock")?;
    let result = async {
        let mut contention_retries = 0usize;
        loop {
            scope_row = repositories::update_runtime_graph_projection_scope(
                &state.persistence.postgres,
                scope_row.id,
                &crate::domains::runtime_graph::RuntimeGraphProjectionLockState::Acquired,
                &crate::domains::runtime_graph::RuntimeGraphProjectionWriteState::Pending,
                contention_retries,
                None,
                None,
            )
            .await
            .context("failed to mark graph projection scope as acquired")?
            .ok_or_else(|| anyhow!("graph projection scope disappeared before write"))?;
            persist_graph_diagnostics_snapshot(
                state,
                scope.library_id,
                pending_node_write_count,
                pending_edge_write_count,
                None,
                None,
                true,
            )
            .await?;

            match operation().await {
                Ok(()) => {
                    scope_row = repositories::update_runtime_graph_projection_scope(
                        &state.persistence.postgres,
                        scope_row.id,
                        &crate::domains::runtime_graph::RuntimeGraphProjectionLockState::Acquired,
                        &crate::domains::runtime_graph::RuntimeGraphProjectionWriteState::Completed,
                        contention_retries,
                        None,
                        Some(Utc::now()),
                    )
                    .await
                    .context("failed to complete graph projection scope")?
                    .ok_or_else(|| {
                        anyhow!("graph projection scope disappeared after successful write")
                    })?;
                    let _ = scope_row;
                    persist_graph_diagnostics_snapshot(
                        state,
                        scope.library_id,
                        0,
                        0,
                        None,
                        None,
                        true,
                    )
                    .await?;
                    return Ok(());
                }
                Err(error) => match guard.classify_write_error(&error, contention_retries + 1) {
                    GraphProjectionFailureDecision::RetryContention => {
                        contention_retries += 1;
                        scope_row = repositories::update_runtime_graph_projection_scope(
                                &state.persistence.postgres,
                                scope_row.id,
                                &crate::domains::runtime_graph::RuntimeGraphProjectionLockState::RetryingContention,
                                &crate::domains::runtime_graph::RuntimeGraphProjectionWriteState::Pending,
                                contention_retries,
                                None,
                                None,
                            )
                            .await
                            .context("failed to persist retryable graph projection contention")?
                            .ok_or_else(|| {
                                anyhow!("graph projection scope disappeared after retryable contention")
                            })?;
                        persist_graph_diagnostics_snapshot(
                                state,
                                scope.library_id,
                                pending_node_write_count,
                                pending_edge_write_count,
                                Some(
                                    crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionContention,
                                ),
                                Some(Utc::now()),
                                true,
                            )
                            .await?;
                        sleep(Duration::from_millis(200)).await;
                    }
                    GraphProjectionFailureDecision::FailExplicitly(failure_kind) => {
                        finalize_projection_scope_failure(
                                state,
                                &mut scope_row,
                                if matches!(
                                    failure_kind,
                                    crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionContention
                                ) {
                                    crate::domains::runtime_graph::RuntimeGraphProjectionLockState::FailedContention
                                } else {
                                    crate::domains::runtime_graph::RuntimeGraphProjectionLockState::Acquired
                                },
                                failure_kind,
                                pending_node_write_count,
                                pending_edge_write_count,
                            )
                            .await?;
                        return Err(anyhow!(error.to_string()));
                    }
                },
            }
        }
    }
    .await;
    let release_result =
        repositories::release_runtime_library_projection_lock(projection_lock, scope.library_id)
            .await
            .context("failed to release graph projection advisory lock");
    match (result, release_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(release_error)) => Err(release_error),
        (Err(error), Err(release_error)) => Err(release_error).context(error.to_string()),
    }
}

async fn finalize_projection_scope_failure(
    state: &AppState,
    scope_row: &mut repositories::RuntimeGraphProjectionScopeRow,
    lock_state: crate::domains::runtime_graph::RuntimeGraphProjectionLockState,
    failure_kind: crate::domains::runtime_graph::RuntimeGraphWriteFailureKind,
    pending_node_write_count: usize,
    pending_edge_write_count: usize,
) -> anyhow::Result<()> {
    *scope_row = repositories::update_runtime_graph_projection_scope(
        &state.persistence.postgres,
        scope_row.id,
        &lock_state,
        &crate::domains::runtime_graph::RuntimeGraphProjectionWriteState::Failed,
        usize::try_from(scope_row.deadlock_retry_count).unwrap_or_default(),
        Some(&failure_kind),
        Some(Utc::now()),
    )
    .await
    .context("failed to persist graph projection failure scope")?
    .ok_or_else(|| anyhow!("graph projection scope disappeared while finalizing failure"))?;
    persist_graph_diagnostics_snapshot(
        state,
        scope_row.project_id,
        pending_node_write_count,
        pending_edge_write_count,
        Some(failure_kind),
        Some(Utc::now()),
        true,
    )
    .await?;
    Ok(())
}

async fn persist_graph_diagnostics_snapshot(
    state: &AppState,
    library_id: Uuid,
    pending_node_write_count: usize,
    pending_edge_write_count: usize,
    explicit_failure_kind: Option<crate::domains::runtime_graph::RuntimeGraphWriteFailureKind>,
    explicit_failure_at: Option<chrono::DateTime<Utc>>,
    is_runtime_readable: bool,
) -> anyhow::Result<()> {
    let scope_counters = repositories::load_runtime_graph_projection_scope_counters(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load graph projection scope counters for diagnostics snapshot")?;
    let snapshot = state.resolve_settle_blockers_services.graph_diagnostics_snapshot.summarize(
        library_id,
        usize::try_from(scope_counters.active_projection_count).unwrap_or_default(),
        usize::try_from(scope_counters.retrying_projection_count).unwrap_or_default(),
        usize::try_from(scope_counters.failed_projection_count).unwrap_or_default(),
        pending_node_write_count,
        pending_edge_write_count,
        explicit_failure_kind.or_else(|| {
            scope_counters
                .last_failure_kind
                .as_deref()
                .and_then(parse_runtime_graph_write_failure_kind)
        }),
        explicit_failure_at.or(scope_counters.last_failure_at),
        is_runtime_readable,
    );
    let previous_snapshot = repositories::load_runtime_graph_diagnostics_snapshot(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load previous runtime graph diagnostics snapshot")?
    .as_ref()
    .map(map_runtime_graph_diagnostics_snapshot_row);
    if !state
        .resolve_settle_blockers_services
        .graph_diagnostics_snapshot
        .should_persist(previous_snapshot.as_ref(), &snapshot)
    {
        return Ok(());
    }
    repositories::upsert_runtime_graph_diagnostics_snapshot(
        &state.persistence.postgres,
        &repositories::RuntimeGraphDiagnosticsSnapshotRow {
            project_id: snapshot.library_id,
            projection_health: runtime_graph_projection_health_key(&snapshot.projection_health)
                .to_string(),
            active_projection_count: i64::try_from(snapshot.active_projection_count)
                .unwrap_or(i64::MAX),
            retrying_projection_count: i64::try_from(snapshot.retrying_projection_count)
                .unwrap_or(i64::MAX),
            failed_projection_count: i64::try_from(snapshot.failed_projection_count)
                .unwrap_or(i64::MAX),
            pending_node_write_count: i64::try_from(snapshot.pending_node_write_count)
                .unwrap_or(i64::MAX),
            pending_edge_write_count: i64::try_from(snapshot.pending_edge_write_count)
                .unwrap_or(i64::MAX),
            last_projection_failure_kind: snapshot
                .last_projection_failure_kind
                .as_ref()
                .map(runtime_graph_write_failure_kind_key)
                .map(str::to_string),
            last_projection_failure_at: snapshot.last_projection_failure_at,
            is_runtime_readable: snapshot.is_runtime_readable,
            snapshot_at: snapshot.snapshot_at,
        },
    )
    .await
    .context("failed to persist runtime graph diagnostics snapshot")?;
    Ok(())
}

fn parse_runtime_graph_write_failure_kind(
    value: &str,
) -> Option<crate::domains::runtime_graph::RuntimeGraphWriteFailureKind> {
    match value {
        "projection_contention" => {
            Some(crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionContention)
        }
        "graph_persistence_integrity" => Some(
            crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::GraphPersistenceIntegrity,
        ),
        "diagnostics_unavailable" => Some(
            crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::DiagnosticsUnavailable,
        ),
        "projection_failure" => {
            Some(crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionFailure)
        }
        _ => None,
    }
}

fn parse_runtime_graph_projection_health(
    value: &str,
) -> Option<crate::domains::runtime_graph::RuntimeGraphProjectionHealth> {
    match value {
        "healthy" => Some(crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Healthy),
        "retrying_contention" => {
            Some(crate::domains::runtime_graph::RuntimeGraphProjectionHealth::RetryingContention)
        }
        "degraded" => Some(crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Degraded),
        "failed" => Some(crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Failed),
        _ => None,
    }
}

fn map_runtime_graph_diagnostics_snapshot_row(
    row: &repositories::RuntimeGraphDiagnosticsSnapshotRow,
) -> crate::domains::runtime_graph::RuntimeGraphDiagnosticsSnapshot {
    crate::domains::runtime_graph::RuntimeGraphDiagnosticsSnapshot {
        library_id: row.project_id,
        snapshot_at: row.snapshot_at,
        projection_health: parse_runtime_graph_projection_health(&row.projection_health)
            .unwrap_or(crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Degraded),
        active_projection_count: usize::try_from(row.active_projection_count).unwrap_or_default(),
        retrying_projection_count: usize::try_from(row.retrying_projection_count)
            .unwrap_or_default(),
        failed_projection_count: usize::try_from(row.failed_projection_count).unwrap_or_default(),
        pending_node_write_count: usize::try_from(row.pending_node_write_count).unwrap_or_default(),
        pending_edge_write_count: usize::try_from(row.pending_edge_write_count).unwrap_or_default(),
        last_projection_failure_kind: row
            .last_projection_failure_kind
            .as_deref()
            .and_then(parse_runtime_graph_write_failure_kind),
        last_projection_failure_at: row.last_projection_failure_at,
        is_runtime_readable: row.is_runtime_readable,
    }
}

fn runtime_graph_projection_health_key(
    value: &crate::domains::runtime_graph::RuntimeGraphProjectionHealth,
) -> &'static str {
    match value {
        crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Healthy => "healthy",
        crate::domains::runtime_graph::RuntimeGraphProjectionHealth::RetryingContention => {
            "retrying_contention"
        }
        crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Degraded => "degraded",
        crate::domains::runtime_graph::RuntimeGraphProjectionHealth::Failed => "failed",
    }
}

fn runtime_graph_write_failure_kind_key(
    value: &crate::domains::runtime_graph::RuntimeGraphWriteFailureKind,
) -> &'static str {
    match value {
        crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionContention => {
            "projection_contention"
        }
        crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::GraphPersistenceIntegrity => {
            "graph_persistence_integrity"
        }
        crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::DiagnosticsUnavailable => {
            "diagnostics_unavailable"
        }
        crate::domains::runtime_graph::RuntimeGraphWriteFailureKind::ProjectionFailure => {
            "projection_failure"
        }
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
            project_id: Uuid::nil(),
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
            project_id: Uuid::nil(),
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
            project_id: Uuid::nil(),
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
