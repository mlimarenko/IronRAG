use super::*;
use crate::services::maintenance::lease::MaintenanceRunState;
use std::io::{Read, Seek, SeekFrom};

fn capture_completion(
    class: MaintenanceClass,
    detail: GcSweepDetail,
    continuing: bool,
    duration: Duration,
) -> serde_json::Value {
    let sink = tempfile::tempfile().expect("completion log sink");
    let mut reader = sink.try_clone().expect("completion log reader");
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_target(false)
        .with_writer(sink)
        .finish();
    let epoch = chrono::DateTime::from_timestamp(0, 0).expect("epoch");
    let run = MaintenanceJobRun {
        id: Uuid::from_u128(1),
        class: class.as_str().to_owned(),
        scope_kind: "library".to_owned(),
        scope_id: Some(Uuid::from_u128(2)),
        owner_node: None,
        state: MaintenanceRunState::Leased,
        cursor_json: if continuing {
            serde_json::json!({"page": 1})
        } else {
            serde_json::json!({})
        },
        attempts: 0,
        last_started_at: Some(epoch),
        heartbeat_at: Some(epoch),
        last_completed_at: None,
        rows_removed_total: 100,
        bytes_reclaimed_total: 200,
        error_code: None,
        error_text: None,
        next_due_at: epoch,
        created_at: epoch,
        updated_at: epoch,
    };
    tracing::subscriber::with_default(subscriber, || {
        log_maintenance_sweep_completed(class, &run, &detail, duration);
    });
    reader.seek(SeekFrom::Start(0)).expect("rewind completion log");
    let mut output = String::new();
    reader.read_to_string(&mut output).expect("read completion log");
    assert_eq!(output.lines().count(), 1, "one event per dispatch: {output}");
    serde_json::from_str::<serde_json::Value>(&output).expect("structured completion event")["fields"]
        .clone()
}

#[test]
fn zero_deletion_completion_is_observed_for_every_scheduled_gc_class() {
    for class in [
        MaintenanceClass::GcSupersededVectors,
        MaintenanceClass::GcRuntimeGraph,
        MaintenanceClass::GcHistoricalProjection,
    ] {
        let fields =
            capture_completion(class, GcSweepDetail::no_work(), false, Duration::from_millis(250));
        assert_eq!(fields["message"], "maintenance sweep completed");
        assert_eq!(fields["class"], class.as_str());
        assert_eq!(fields["rows_removed"], 0);
        assert_eq!(fields["bytes_reclaimed"], 0);
        assert_eq!(fields["continuing_sweep"], false);
        assert_eq!(fields["work_remaining"], false);
        assert_eq!(fields["duration_seconds"], 0.25);
    }
}

#[test]
fn productive_completion_reports_dispatch_delta_not_lifetime_total() {
    let detail =
        GcSweepDetail { rows_removed: 17, bytes_reclaimed: 42, ..GcSweepDetail::no_work() };
    let fields =
        capture_completion(MaintenanceClass::GcRuntimeGraph, detail, false, Duration::from_secs(2));
    assert_eq!(fields["rows_removed"], 17);
    assert_eq!(fields["bytes_reclaimed"], 42);
    assert_eq!(fields["duration_seconds"], 2.0);
    assert_eq!(fields["run_id"], Uuid::from_u128(1).to_string());
}

#[test]
fn zero_deletion_cursor_batch_is_not_presented_as_a_complete_sweep() {
    let detail = GcSweepDetail { work_remaining: true, ..GcSweepDetail::no_work() };
    let fields =
        capture_completion(MaintenanceClass::GcRuntimeGraph, detail, true, Duration::from_secs(3));
    assert_eq!(fields["rows_removed"], 0);
    assert_eq!(fields["continuing_sweep"], true);
    assert_eq!(fields["work_remaining"], true);
    assert_eq!(fields["duration_seconds"], 3.0);
}

#[test]
fn final_cursor_batch_retains_its_continuation_identity() {
    let fields = capture_completion(
        MaintenanceClass::GcSupersededVectors,
        GcSweepDetail::no_work(),
        true,
        Duration::ZERO,
    );
    assert_eq!(fields["continuing_sweep"], true);
    assert_eq!(fields["work_remaining"], false);
}
