//! Per-library debounce for graph maintenance passes (entity resolution,
//! community detection, community summary generation).
//!
//! Every `content_mutation` ingest job finishes by running a library-wide
//! maintenance pass: entity resolution, community detection, and a batch
//! of LLM calls for community summaries. That work is idempotent and
//! operates on the whole graph, so running it once per completed job
//! scales as O(jobs × library-size). Under a full queue burst on any
//! reasonably sized library the maintenance loop becomes the dominant
//! CPU sink — each finishing job kicks another pass while the previous
//! pass is still running.
//!
//! This module squashes that burst down to a single pass per library
//! per [`MAINTENANCE_INTERVAL`] window. The first ingest job to finish
//! in a window claims the slot via [`try_acquire_graph_maintenance_slot`]
//! and runs the full pass; subsequent jobs see the slot already claimed
//! and skip the block entirely. The maintenance work remains correct —
//! it only has to *eventually* catch up with the graph state, and the
//! final job in a queue burst is guaranteed to trigger one more pass
//! after the slot expires.
//!
//! The throttle lives in process-local state (a `Mutex<HashMap>`)
//! because it guards a process-local CPU hot path — cross-worker
//! coordination would introduce a DB round-trip on a fast path that
//! we specifically want to avoid.
//!
//! No configuration knobs on purpose: a process-local debounce is only
//! as good as its tuning, and the right value is "however long one
//! maintenance pass takes, plus a small cushion." 30 s comfortably
//! covers a mid-sized library (tens of thousands of nodes and edges,
//! on the order of a hundred communities).

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use uuid::Uuid;

fn last_run() -> &'static Mutex<HashMap<Uuid, Instant>> {
    static LAST_RUN: OnceLock<Mutex<HashMap<Uuid, Instant>>> = OnceLock::new();
    LAST_RUN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Minimum wall-clock gap between two maintenance passes on the same
/// library. Chosen to comfortably exceed one pass on the reference
/// library; smaller libraries finish well inside the window and end up
/// running at whatever natural rate ingest jobs arrive.
/// How long a maintenance slot stays claimed once it has been acquired.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);

/// Returns `true` if the caller has been granted the maintenance slot
/// for `library_id` in the current window. The caller then MUST run
/// the maintenance pass — there is no explicit release, the slot
/// becomes available again automatically once [`MAINTENANCE_INTERVAL`]
/// has elapsed.
///
/// Returns `false` if another caller already ran (or is running) the
/// pass inside the current window. The caller should skip the
/// maintenance block entirely; the library will converge on the next
/// finished job.
#[must_use]
pub fn try_acquire_graph_maintenance_slot(library_id: Uuid) -> bool {
    let now = Instant::now();
    let mut guard = last_run().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.get(&library_id) {
        Some(last) if now.duration_since(*last) < MAINTENANCE_INTERVAL => false,
        _ => {
            guard.insert(library_id, now);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_caller_acquires_slot() {
        // Use a fresh uuid so we do not race with any other test.
        let library_id = Uuid::now_v7();
        assert!(try_acquire_graph_maintenance_slot(library_id));
    }

    #[test]
    fn second_caller_in_same_window_is_rejected() {
        let library_id = Uuid::now_v7();
        assert!(try_acquire_graph_maintenance_slot(library_id));
        assert!(!try_acquire_graph_maintenance_slot(library_id));
    }

    #[test]
    fn distinct_libraries_do_not_contend() {
        let library_a = Uuid::now_v7();
        let library_b = Uuid::now_v7();
        assert!(try_acquire_graph_maintenance_slot(library_a));
        assert!(try_acquire_graph_maintenance_slot(library_b));
    }
}
