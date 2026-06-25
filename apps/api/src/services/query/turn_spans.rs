//! Per-execution span capture for the debug inspector.
//!
//! A lightweight, task-local sink records timed spans (DB queries, retrieval
//! lanes, …) during one query execution so the UI can show exactly where time
//! went and operators can spot heavy sections and catch regressions over time.
//!
//! Design notes:
//! - Zero cost when no sink is active: external/non-debug callers and code
//!   paths outside a `capture_turn_spans` scope hit a cheap `try_with` miss.
//! - Propagates across the *same-task* parallelism the query path uses
//!   (`tokio::join!` / `try_join!` / `futures::buffer_unordered`); it does not
//!   cross a `tokio::spawn` boundary, which the hot retrieval/answer path does
//!   not use.
//! - The sink is drained once at the end of an execution and persisted on that
//!   execution's `LlmContextSnapshot`, so the parent agent turn and each child
//!   `grounded_answer` execution each carry their own spans.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One timed sub-operation within an execution.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnSpan {
    /// Operator-facing label, e.g. `db.cursor`, `retrieve.vector`.
    pub name: String,
    /// Coarse category for grouping/colour: `db` | `lane` | `stage` | `llm`.
    pub kind: String,
    pub duration_ms: u64,
    /// Offset from execution start, for ordering on a timeline.
    pub started_offset_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

/// Upper bound on spans kept per execution; guards against a pathological loop
/// flooding the snapshot. Heavy sections are what matter, so once the cap is
/// hit further (typically fast, repetitive) spans are dropped.
const MAX_SPANS_PER_TURN: usize = 1024;

pub struct TurnSpanSink {
    started: Instant,
    spans: Mutex<Vec<TurnSpan>>,
}

impl TurnSpanSink {
    fn new() -> Self {
        Self { started: Instant::now(), spans: Mutex::new(Vec::new()) }
    }

    fn record(
        &self,
        name: impl Into<String>,
        kind: &str,
        duration_ms: u64,
        detail: Option<String>,
        rows: Option<u64>,
    ) {
        let elapsed_ms = self.started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let started_offset_ms = elapsed_ms.saturating_sub(duration_ms);
        if let Ok(mut spans) = self.spans.lock() {
            if spans.len() < MAX_SPANS_PER_TURN {
                spans.push(TurnSpan {
                    name: name.into(),
                    kind: kind.to_string(),
                    duration_ms,
                    started_offset_ms,
                    detail,
                    rows,
                });
            }
        }
    }

    fn drain(&self) -> Vec<TurnSpan> {
        self.spans.lock().map(|mut spans| std::mem::take(&mut *spans)).unwrap_or_default()
    }
}

tokio::task_local! {
    static TURN_SPANS: Arc<TurnSpanSink>;
}

/// Record a span into the current execution's sink, if one is active.
/// No-op when called outside a [`capture_turn_spans`] scope.
pub fn record_span(
    name: impl Into<String>,
    kind: &str,
    duration_ms: u64,
    detail: Option<String>,
    rows: Option<u64>,
) {
    let _ = TURN_SPANS.try_with(|sink| sink.record(name, kind, duration_ms, detail, rows));
}

/// Run `fut` with a fresh span sink scoped to the current execution and return
/// its output plus the spans recorded during it.
pub async fn capture_turn_spans<F, T>(fut: F) -> (T, Vec<TurnSpan>)
where
    F: std::future::Future<Output = T>,
{
    let sink = Arc::new(TurnSpanSink::new());
    let result = TURN_SPANS.scope(sink.clone(), fut).await;
    (result, sink.drain())
}

/// Hand-off store keyed by `execution_id`. The retrieval flow drains its span
/// sink and stashes the spans here once the execution_id is known; the snapshot
/// writer takes them when persisting that execution's snapshot. This decouples
/// recording (task-local, scoped to retrieval) from persistence (a later,
/// separate code path) without threading spans through the whole answer flow.
static SPAN_STORE: LazyLock<Mutex<HashMap<Uuid, Vec<TurnSpan>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Bound the store so an execution whose snapshot is never written (rare error
/// paths) cannot leak unboundedly.
const MAX_STASHED_EXECUTIONS: usize = 256;

/// Stash spans for `execution_id` so the snapshot writer can attach them later.
pub fn stash_execution_spans(execution_id: Uuid, spans: Vec<TurnSpan>) {
    if spans.is_empty() {
        return;
    }
    if let Ok(mut store) = SPAN_STORE.lock() {
        if store.len() >= MAX_STASHED_EXECUTIONS {
            store.clear();
        }
        store.insert(execution_id, spans);
    }
}

/// Take (and remove) the stashed spans for `execution_id`. Empty if none.
pub fn take_execution_spans(execution_id: Uuid) -> Vec<TurnSpan> {
    SPAN_STORE.lock().ok().and_then(|mut store| store.remove(&execution_id)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_spans_recorded_within_scope() {
        let (_out, spans) = capture_turn_spans(async {
            record_span("db.cursor", "db", 12, Some("knowledge_chunk".into()), Some(24));
            record_span("retrieve.vector", "lane", 30, None, None);
        })
        .await;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "db.cursor");
        assert_eq!(spans[0].kind, "db");
        assert_eq!(spans[0].rows, Some(24));
        assert_eq!(spans[1].name, "retrieve.vector");
    }

    #[tokio::test]
    async fn record_span_outside_scope_is_noop() {
        // Must not panic when no sink is active.
        record_span("orphan", "db", 1, None, None);
    }

    #[tokio::test]
    async fn captures_spans_across_join_boundary() {
        // The retrieval hot path fans lanes out with `tokio::join!`, which polls
        // every future on the *same* task — so the task-local sink must be
        // visible to all of them. This is the invariant the `retrieve.*` lane
        // spans rely on.
        let (_out, spans) = capture_turn_spans(async {
            let first = async { record_span("retrieve.vector", "lane", 5, None, Some(8)) };
            let second = async { record_span("retrieve.lexical", "lane", 7, None, Some(4)) };
            tokio::join!(first, second);
        })
        .await;
        let mut names: Vec<&str> = spans.iter().map(|span| span.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["retrieve.lexical", "retrieve.vector"]);
    }

    #[tokio::test]
    async fn record_span_does_not_cross_spawn_boundary() {
        // `tokio::spawn` starts a fresh task with its own task-local scope, so
        // the sink does not propagate into it. Documenting the boundary keeps the
        // hot path honest about avoiding `tokio::spawn` between capture and record.
        let (_out, spans) = capture_turn_spans(async {
            record_span("same.task", "db", 1, None, None);
            tokio::spawn(async {
                record_span("spawned.task", "db", 1, None, None);
            })
            .await
            .expect("spawned task joins");
        })
        .await;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "same.task");
    }

    #[test]
    fn stash_and_take_execution_spans_round_trip() {
        let execution_id = Uuid::new_v4();
        stash_execution_spans(
            execution_id,
            vec![TurnSpan {
                name: "db.knowledge_chunk".into(),
                kind: "db".into(),
                duration_ms: 5,
                started_offset_ms: 0,
                detail: None,
                rows: Some(3),
            }],
        );
        let taken = take_execution_spans(execution_id);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].name, "db.knowledge_chunk");
        // The store removes the entry on take, so a second take is empty.
        assert!(take_execution_spans(execution_id).is_empty());
    }

    #[test]
    fn stash_empty_spans_is_noop() {
        let execution_id = Uuid::new_v4();
        stash_execution_spans(execution_id, Vec::new());
        assert!(take_execution_spans(execution_id).is_empty());
    }
}
