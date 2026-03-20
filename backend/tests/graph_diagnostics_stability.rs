use chrono::{Duration, Utc};
use uuid::Uuid;

use rustrag_backend::domains::runtime_graph::{
    RuntimeGraphDiagnosticsSnapshot, RuntimeGraphProjectionHealth, RuntimeGraphWriteFailureKind,
};
use rustrag_backend::services::graph_diagnostics_snapshot::GraphDiagnosticsSnapshotService;

fn base_snapshot() -> RuntimeGraphDiagnosticsSnapshot {
    RuntimeGraphDiagnosticsSnapshot {
        library_id: Uuid::now_v7(),
        snapshot_at: Utc::now(),
        projection_health: RuntimeGraphProjectionHealth::Healthy,
        active_projection_count: 0,
        retrying_projection_count: 0,
        failed_projection_count: 0,
        pending_node_write_count: 0,
        pending_edge_write_count: 0,
        last_projection_failure_kind: None,
        last_projection_failure_at: None,
        is_runtime_readable: true,
    }
}

#[test]
fn identical_graph_snapshot_is_not_repersisted_inside_cadence_window() {
    let service = GraphDiagnosticsSnapshotService::new(30);
    let previous = base_snapshot();
    let next = RuntimeGraphDiagnosticsSnapshot {
        snapshot_at: previous.snapshot_at + Duration::seconds(4),
        ..previous.clone()
    };

    assert!(!service.should_persist(Some(&previous), &next));
}

#[test]
fn degraded_graph_snapshot_is_persisted_immediately_when_failure_class_changes() {
    let service = GraphDiagnosticsSnapshotService::new(30);
    let previous = base_snapshot();
    let next = RuntimeGraphDiagnosticsSnapshot {
        projection_health: RuntimeGraphProjectionHealth::Failed,
        failed_projection_count: 1,
        pending_edge_write_count: 3,
        last_projection_failure_kind: Some(RuntimeGraphWriteFailureKind::ProjectionFailure),
        last_projection_failure_at: Some(previous.snapshot_at + Duration::seconds(1)),
        snapshot_at: previous.snapshot_at + Duration::seconds(1),
        ..previous.clone()
    };

    assert!(service.should_persist(Some(&previous), &next));
}
