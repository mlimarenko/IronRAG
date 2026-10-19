use super::*;

use crate::services::graph::projection::GraphSnapshotState;

/// Projection strategy for one revision-graph reconcile.
///
/// `TargetedSupersede` and `TargetedFresh` both republish only the affected
/// subgraph (bounded by the document's own node/edge footprint); the difference
/// is that a supersede first prunes the old revision's now-orphaned rows.
/// `Full` and `Preserve`/`Empty` keep the prior behavior for the first-ever
/// graph build and genuine no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RevisionReconcileProjectionPlan {
    TargetedSupersede,
    TargetedFresh,
    Full,
    Preserve,
    Empty,
}

impl RevisionReconcileProjectionPlan {
    /// A library-wide source-truth change needs a broad active-summary
    /// supersede (the inline regeneration self-skips for large graphs). Targeted
    /// refreshes can scope the summary supersede to their own contributions.
    pub(super) const fn broad_summary_refresh(self) -> bool {
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
pub(super) const fn revision_reconcile_projection_plan(
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

pub(super) fn union_targeted_ids(
    changed_ids: &[Uuid],
    superseded_ids: &BTreeSet<Uuid>,
) -> Vec<Uuid> {
    let mut union = superseded_ids.clone();
    union.extend(changed_ids.iter().copied());
    union.into_iter().collect()
}

pub(super) fn is_graph_reconcile_chunk_text_eligible(text: &str) -> bool {
    is_graph_extraction_text_eligible(text)
}

/// Inputs that select and scope one authorized revision projection.
///
/// Groups the prior snapshot, chosen plan and targeted id sets so the entry
/// point stays within the argument budget; nothing about the plan changes.
pub(super) struct AuthorizedRevisionProjectionInput<'a> {
    pub(super) authority: IngestRevisionWriteAuthority,
    pub(super) document_id: Uuid,
    pub(super) snapshot: Option<&'a RuntimeGraphSnapshotRow>,
    pub(super) plan: RevisionReconcileProjectionPlan,
    pub(super) targeted_node_ids: &'a [Uuid],
    pub(super) targeted_edge_ids: &'a [Uuid],
}

pub(super) async fn execute_authorized_revision_projection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    input: AuthorizedRevisionProjectionInput<'_>,
    cancellation_token: &CancellationToken,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let AuthorizedRevisionProjectionInput {
        authority,
        document_id,
        snapshot,
        plan,
        targeted_node_ids,
        targeted_edge_ids,
    } = input;
    match plan {
        RevisionReconcileProjectionPlan::TargetedSupersede => {
            repositories::recalculate_runtime_graph_support_counts_by_ids_with_executor(
                connection,
                scope.library_id,
                scope.projection_version,
                targeted_node_ids,
                targeted_edge_ids,
            )
            .await?;
            ensure_not_cancelled(cancellation_token)?;
            repositories::prune_runtime_graph_projection_rows_without_support_with_connection(
                connection,
                scope.library_id,
                scope.projection_version,
                targeted_node_ids,
                targeted_edge_ids,
            )
            .await?;
            project_authorized_snapshot(connection, scope, authority, document_id, false).await
        }
        RevisionReconcileProjectionPlan::TargetedFresh => {
            project_authorized_snapshot(connection, scope, authority, document_id, false).await
        }
        RevisionReconcileProjectionPlan::Full => {
            repositories::synchronize_runtime_graph_support_and_prune_with_connection(
                connection,
                scope.library_id,
                scope.projection_version,
            )
            .await?;
            project_authorized_snapshot(connection, scope, authority, document_id, false).await
        }
        RevisionReconcileProjectionPlan::Preserve => {
            authorized_preserve_or_empty_snapshot(connection, scope, snapshot).await
        }
        RevisionReconcileProjectionPlan::Empty => {
            project_authorized_snapshot(connection, scope, authority, document_id, true).await
        }
    }
}

pub(super) async fn authorized_preserve_or_empty_snapshot(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    snapshot: Option<&RuntimeGraphSnapshotRow>,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    if let Some(snapshot) = snapshot {
        let terminal_state = preserved_snapshot_terminal_state(snapshot);
        persist_authorized_snapshot(
            connection,
            scope,
            GraphSnapshotState {
                graph_status: "building",
                node_count: snapshot.node_count,
                edge_count: snapshot.edge_count,
                provenance_coverage_percent: snapshot.provenance_coverage_percent,
                last_error_message: terminal_state.last_error_message,
            },
        )
        .await?;
        persist_authorized_snapshot(
            connection,
            scope,
            GraphSnapshotState {
                graph_status: terminal_state.graph_status,
                node_count: snapshot.node_count,
                edge_count: snapshot.edge_count,
                provenance_coverage_percent: snapshot.provenance_coverage_percent,
                last_error_message: terminal_state.last_error_message,
            },
        )
        .await?;
        return Ok(GraphProjectionOutcome {
            projection_version: scope.projection_version,
            node_count: usize::try_from(snapshot.node_count).unwrap_or_default(),
            edge_count: usize::try_from(snapshot.edge_count).unwrap_or_default(),
            graph_status: terminal_state.graph_status.to_string(),
        });
    }
    persist_projected_snapshot_counts(connection, scope, 0, 0).await
}

pub(super) struct PreservedSnapshotTerminalState<'a> {
    pub(super) graph_status: &'static str,
    pub(super) last_error_message: Option<&'a str>,
}

pub(super) fn preserved_snapshot_terminal_state(
    snapshot: &RuntimeGraphSnapshotRow,
) -> PreservedSnapshotTerminalState<'_> {
    if snapshot.graph_status == "failed" {
        return PreservedSnapshotTerminalState {
            graph_status: "failed",
            last_error_message: snapshot.last_error_message.as_deref(),
        };
    }
    PreservedSnapshotTerminalState {
        graph_status: if snapshot.node_count <= 0 && snapshot.edge_count <= 0 {
            GRAPH_STATUS_EMPTY
        } else {
            GRAPH_STATUS_READY
        },
        last_error_message: None,
    }
}

async fn project_authorized_snapshot(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
    force_empty: bool,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let counts = repositories::count_authorized_runtime_graph_projection_with_connection(
        &mut *connection,
        authority,
        document_id,
        scope.projection_version,
    )
    .await?;
    let node_count = if force_empty {
        0
    } else {
        i32::try_from(counts.node_count.max(0)).map_err(|_| {
            GraphServiceError::PersistenceIntegrity {
                message: "runtime graph node count exceeds snapshot capacity".to_string(),
            }
        })?
    };
    let edge_count = if force_empty {
        0
    } else {
        i32::try_from(counts.edge_count.max(0)).map_err(|_| {
            GraphServiceError::PersistenceIntegrity {
                message: "runtime graph edge count exceeds snapshot capacity".to_string(),
            }
        })?
    };
    persist_projected_snapshot_counts(connection, scope, node_count, edge_count).await
}

async fn persist_projected_snapshot_counts(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    node_count: i32,
    edge_count: i32,
) -> Result<GraphProjectionOutcome, GraphServiceError> {
    let previous_coverage =
        repositories::get_runtime_graph_snapshot_with_executor(&mut *connection, scope.library_id)
            .await?
            .and_then(|snapshot| snapshot.provenance_coverage_percent);
    let graph_status =
        if node_count == 0 && edge_count == 0 { GRAPH_STATUS_EMPTY } else { GRAPH_STATUS_READY };
    persist_authorized_snapshot(
        connection,
        scope,
        GraphSnapshotState {
            graph_status: "building",
            node_count,
            edge_count,
            provenance_coverage_percent: previous_coverage,
            last_error_message: None,
        },
    )
    .await?;
    persist_authorized_snapshot(
        connection,
        scope,
        GraphSnapshotState {
            graph_status,
            node_count,
            edge_count,
            provenance_coverage_percent: previous_coverage,
            last_error_message: None,
        },
    )
    .await?;
    Ok(GraphProjectionOutcome {
        projection_version: scope.projection_version,
        node_count: usize::try_from(node_count).unwrap_or_default(),
        edge_count: usize::try_from(edge_count).unwrap_or_default(),
        graph_status: graph_status.to_string(),
    })
}

async fn persist_authorized_snapshot(
    connection: &mut sqlx::PgConnection,
    scope: &GraphProjectionScope,
    snapshot: GraphSnapshotState<'_>,
) -> Result<(), GraphServiceError> {
    let GraphSnapshotState {
        graph_status,
        node_count,
        edge_count,
        provenance_coverage_percent,
        last_error_message,
    } = snapshot;
    repositories::upsert_runtime_graph_snapshot_with_executor(
        &mut *connection,
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
    )
    .await?
    .ok_or_else(|| GraphServiceError::StateConflict {
        message: format!(
            "authorized runtime graph snapshot conflict for library {} projection {} build {}",
            scope.library_id, scope.projection_version, scope.build_epoch
        ),
    })?;
    Ok(())
}
