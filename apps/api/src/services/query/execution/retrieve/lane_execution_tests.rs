use super::{FOCUS_BROADEN_MIN_CHUNKS, record_stage_elapsed, should_broaden_focus, timed_stage};
use crate::services::query::turn_spans::capture_turn_spans;

#[test]
fn inferred_pin_broadens_only_when_coverage_is_at_or_below_floor() {
    // A thin inferred-focus pin (e.g. a 2-chunk same-titled stub) broadens.
    assert!(should_broaden_focus(true, 0));
    assert!(should_broaden_focus(true, FOCUS_BROADEN_MIN_CHUNKS));
    // A focused document with sufficient coverage keeps its pin.
    assert!(!should_broaden_focus(true, FOCUS_BROADEN_MIN_CHUNKS + 1));
}

#[test]
fn non_inferred_pin_never_broadens() {
    // A user-referenced (non-inferred) pin is honored even when thin.
    assert!(!should_broaden_focus(false, 0));
    assert!(!should_broaden_focus(false, FOCUS_BROADEN_MIN_CHUNKS));
}

#[tokio::test]
async fn timed_stage_records_stage_span_and_returns_future_output() {
    // The retrieve subtree records its non-chunk phases as `stage` spans. The
    // wrapper must both pass the future's output through untouched and land one
    // span with its own name in the active inspector sink.
    let (output, spans) = capture_turn_spans(async {
        timed_stage("retrieve.test_stage", async { 41_u32 + 1 }).await
    })
    .await;
    assert_eq!(output, 42);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "retrieve.test_stage");
    assert_eq!(spans[0].kind, "stage");
}

#[tokio::test]
async fn record_stage_elapsed_records_named_stage_span() {
    let ((), spans) = capture_turn_spans(async {
        let started = std::time::Instant::now();
        record_stage_elapsed("retrieve.test_stage_elapsed", started);
    })
    .await;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "retrieve.test_stage_elapsed");
    assert_eq!(spans[0].kind, "stage");
}
