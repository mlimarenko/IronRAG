use chrono::{DateTime, Utc};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};
use uuid::Uuid;

use crate::domains::ingest::WebRunCounts;

pub fn init(filter: &str) {
    let _ = fmt().with_env_filter(EnvFilter::new(filter)).with_target(false).try_init();
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
