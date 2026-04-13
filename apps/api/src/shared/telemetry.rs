use chrono::{DateTime, Utc};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};
use uuid::Uuid;

use crate::domains::ingest::WebRunCounts;

/// Third-party crates whose `debug`/`trace` output is extremely noisy and
/// has repeatedly OOM-killed the worker: `scraper`/`html5ever` emit one
/// DEBUG line per HTML tag and per CSS-selector match, which at a 4 MB
/// Confluence export means millions of formatted tracing events. `sqlx`,
/// `hyper`, `h2`, `reqwest`, `rustls`, `mio`, `tower`, `tonic`, `selectors`
/// are similarly chatty. We force them to `warn` regardless of the
/// top-level filter so that raising the application filter to `debug` for
/// troubleshooting doesn't accidentally DoS the worker via its own logs.
const NOISY_CRATE_CEILINGS: &[&str] = &[
    "scraper=warn",
    "html5ever=warn",
    "selectors=warn",
    "hyper=warn",
    "hyper_util=warn",
    "h2=warn",
    "reqwest=warn",
    "rustls=warn",
    "tokio_util=warn",
    "mio=warn",
    "tower=warn",
    "tonic=warn",
    "sqlx=warn",
    "sqlx::query=warn",
    "tungstenite=warn",
    "tokio_tungstenite=warn",
];

pub fn init(filter: &str) {
    let composed = compose_env_filter(filter);
    let _ = fmt().with_env_filter(composed).with_target(false).try_init();
}

/// Reads `/proc/self/status` and returns the current process resident set
/// size (VmRSS) in bytes. Returns None on non-Linux or parse error.
#[must_use]
pub fn current_process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .trim()
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Detects the effective memory ceiling the process runs under: either the
/// cgroup limit (Docker / Kubernetes / systemd slice) or host RAM as a
/// fallback for bare-metal deployments. Returns None when nothing can be
/// read (non-Linux, exotic FS layout).
///
/// Resolution order:
/// 1. cgroup v2 — `/sys/fs/cgroup/memory.max` (`"max"` → None here, caller
///    falls back to host RAM)
/// 2. cgroup v1 — `/sys/fs/cgroup/memory/memory.limit_in_bytes`
/// 3. host total — `/proc/meminfo` `MemTotal`
///
/// Cgroup v1's `limit_in_bytes` reports `9223372036854771712` (≈ 8 EiB)
/// when no limit is set; we treat anything above 1 EiB as "unlimited".
#[must_use]
pub fn detect_container_memory_limit_bytes() -> Option<u64> {
    const UNLIMITED_THRESHOLD: u64 = 1 << 60;

    // cgroup v2
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let trimmed = raw.trim();
        if trimmed != "max" {
            if let Ok(bytes) = trimmed.parse::<u64>() {
                if bytes > 0 && bytes < UNLIMITED_THRESHOLD {
                    return Some(bytes);
                }
            }
        }
    }

    // cgroup v1
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        if let Ok(bytes) = raw.trim().parse::<u64>() {
            if bytes > 0 && bytes < UNLIMITED_THRESHOLD {
                return Some(bytes);
            }
        }
    }

    // Host RAM fallback (bare metal, cgroup unlimited, or non-standard layout).
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb_str) = rest.trim().split_whitespace().next() {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        return Some(kb * 1024);
                    }
                }
            }
        }
    }

    None
}

/// Resolves the memory soft limit (MiB) the ingest dispatcher should use as
/// backpressure. When `explicit_mib > 0` the caller's config wins. Otherwise
/// we take 90% of the detected container/host memory ceiling, so deployments
/// of any container size automatically get a sensible throttle without any
/// hand-tuned config. When detection fails we return 0 (throttle disabled)
/// rather than guessing — the worker will still respect the static
/// parallelism limits and the cgroup OOM kill remains the hard backstop.
#[must_use]
pub fn resolve_memory_soft_limit_mib(explicit_mib: u64) -> u64 {
    if explicit_mib > 0 {
        return explicit_mib;
    }
    let Some(limit_bytes) = detect_container_memory_limit_bytes() else {
        return 0;
    };
    let limit_mib = limit_bytes / (1024 * 1024);
    // 90% headroom. 10% buffer below the hard cgroup limit so that whatever
    // tolerance the kernel OOM killer gives us (plus any stray allocations
    // outside the dispatcher's visibility) still has room before it fires.
    limit_mib.saturating_mul(9) / 10
}

/// Logs the current worker RSS alongside a stage tag. Emitted at INFO so
/// ingest telemetry is easy to follow in `docker logs` without bumping the
/// global filter. Callers pass a short stable tag so the log stream can be
/// grep-ed into a per-stage memory timeline.
pub fn log_worker_rss(tag: &'static str, context: impl std::fmt::Display) {
    if let Some(bytes) = current_process_rss_bytes() {
        let mib = bytes / (1024 * 1024);
        info!(stage = "rss", tag, rss_mib = mib, context = %context, "worker rss");
    }
}

fn compose_env_filter(filter: &str) -> EnvFilter {
    let mut directives = String::from(filter);
    for ceiling in NOISY_CRATE_CEILINGS {
        directives.push(',');
        directives.push_str(ceiling);
    }
    EnvFilter::new(directives)
}

pub fn web_run_event(
    event: &str,
    run_id: Uuid,
    library_id: Uuid,
    mode: &str,
    run_state: &str,
    seed_url: &str,
) {
    info!(
        event,
        %run_id,
        %library_id,
        mode,
        run_state,
        seed_url,
        "web ingest run event"
    );
}

pub fn web_candidate_event(
    event: &str,
    run_id: Uuid,
    candidate_id: Uuid,
    candidate_state: &str,
    normalized_url: &str,
    depth: i32,
    classification_reason: Option<&str>,
    host_classification: Option<&str>,
) {
    info!(
        event,
        %run_id,
        %candidate_id,
        candidate_state,
        normalized_url,
        depth,
        classification_reason = ?classification_reason,
        host_classification = ?host_classification,
        "web ingest candidate event"
    );
}

pub fn web_failure_event(
    event: &str,
    run_id: Uuid,
    candidate_id: Option<Uuid>,
    failure_code: &str,
    classification_reason: Option<&str>,
    final_url: Option<&str>,
    content_type: Option<&str>,
    http_status: Option<i32>,
) {
    warn!(
        event,
        %run_id,
        candidate_id = ?candidate_id.map(|value| value.to_string()),
        failure_code,
        classification_reason = ?classification_reason,
        final_url = ?final_url,
        content_type = ?content_type,
        http_status = ?http_status,
        "web ingest failure"
    );
}

pub fn web_cancel_event(
    event: &str,
    run_id: Uuid,
    library_id: Uuid,
    run_state: &str,
    cancel_requested_at: Option<DateTime<Utc>>,
    counts: &WebRunCounts,
) {
    info!(
        event,
        %run_id,
        %library_id,
        run_state,
        cancel_requested_at = ?cancel_requested_at.map(|value| value.to_rfc3339()),
        discovered = counts.discovered,
        eligible = counts.eligible,
        queued = counts.queued,
        processing = counts.processing,
        processed = counts.processed,
        failed = counts.failed,
        canceled = counts.canceled,
        "web ingest cancellation accepted"
    );
}
