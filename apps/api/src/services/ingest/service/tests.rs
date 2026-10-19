use super::finalization::attempt_finalization_owns_linked_operation;
use super::progress::{
    IngestProgressProfile, ingest_attempt_stage_unit_progress_percent, ingest_progress_profile,
    ingest_stage_progress_percent_for_profile, ingest_stage_unit_progress_percent_for_profile,
    normalize_stage_name, progress_profile_from_stage_details,
};
use super::{
    INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK, INGEST_STAGE_EXTRACT_CONTENT,
    INGEST_STAGE_EXTRACT_GRAPH, INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
    INGEST_STAGE_PREPARE_STRUCTURE, INGEST_STAGE_WEB_DISCOVERY, INGEST_STAGE_WEB_MATERIALIZE_PAGE,
    ingest_stage_metadata, is_retry_budget_exhausted, next_job_queue_state_after_finalize,
    retry_backoff_with_deferral,
};
use crate::{
    domains::ingest::{MAX_INGEST_ATTEMPTS, retry_backoff_after_attempt},
    infra::repositories::ingest_repository::IngestStageEventRow,
};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

fn stage_event(stage_name: &str, details_json: serde_json::Value) -> IngestStageEventRow {
    IngestStageEventRow {
        id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        stage_name: stage_name.to_string(),
        stage_state: "completed".to_string(),
        ordinal: 1,
        message: None,
        details_json,
        recorded_at: Utc::now(),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: None,
        started_at: None,
    }
}

#[test]
fn normalizes_and_accepts_new_supported_stage_names() {
    assert_eq!(
        normalize_stage_name("  Prepare_Structure ").expect("prepare_structure should normalize"),
        INGEST_STAGE_PREPARE_STRUCTURE
    );
    assert_eq!(
        normalize_stage_name("extract_technical_facts")
            .expect("extract_technical_facts should be supported"),
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS
    );
    assert_eq!(
        normalize_stage_name("WEB_DISCOVERY").expect("web_discovery should normalize"),
        INGEST_STAGE_WEB_DISCOVERY
    );
    assert_eq!(
        normalize_stage_name("web_materialize_page")
            .expect("web_materialize_page should be supported"),
        INGEST_STAGE_WEB_MATERIALIZE_PAGE
    );
}

#[test]
fn rejects_unknown_stage_names() {
    let error = normalize_stage_name("legacy_stage").expect_err("legacy stage must be rejected");
    assert_eq!(error.kind(), "bad_request");
}

#[test]
fn exposes_ranked_stage_metadata() {
    let metadata =
        ingest_stage_metadata(INGEST_STAGE_EXTRACT_TECHNICAL_FACTS).expect("metadata should exist");
    assert_eq!(metadata.lifecycle_kind, "grounding");
    assert_eq!(metadata.stage_rank, 40);

    let embed_metadata =
        ingest_stage_metadata(INGEST_STAGE_EMBED_CHUNK).expect("metadata should exist");
    assert_eq!(embed_metadata.lifecycle_kind, "embedding");
    assert_eq!(embed_metadata.stage_rank, 50);

    let web_metadata =
        ingest_stage_metadata(INGEST_STAGE_WEB_DISCOVERY).expect("metadata should exist");
    assert_eq!(web_metadata.lifecycle_kind, "web_discovery");
    assert_eq!(web_metadata.stage_rank, 15);
}

#[test]
fn exposes_content_mutation_stage_progress() {
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::Balanced,
            INGEST_STAGE_EXTRACT_CONTENT,
            "started"
        ),
        Some(2)
    );
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::Balanced,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
            "completed"
        ),
        Some(28)
    );
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::Balanced,
            INGEST_STAGE_FINALIZING,
            "completed"
        ),
        Some(100)
    );
}

#[test]
fn exposes_profile_aware_content_mutation_stage_progress() {
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::InlineText,
            INGEST_STAGE_EXTRACT_CONTENT,
            "completed"
        ),
        Some(4)
    );
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::InlineText,
            INGEST_STAGE_EMBED_CHUNK,
            "completed"
        ),
        Some(55)
    );
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::InlineText,
            INGEST_STAGE_EXTRACT_GRAPH,
            "started"
        ),
        Some(60)
    );
    assert_eq!(
        ingest_stage_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            "completed"
        ),
        Some(30)
    );
}

#[test]
fn infers_progress_profile_from_extraction_details() {
    assert_eq!(
        progress_profile_from_stage_details(&json!({ "source": "knowledge_revision" })),
        Some(IngestProgressProfile::InlineText)
    );
    assert_eq!(
        progress_profile_from_stage_details(&json!({ "fileKind": "text_like" })),
        Some(IngestProgressProfile::InlineText)
    );
    assert_eq!(
        progress_profile_from_stage_details(&json!({ "fileKind": "pdf", "pageCount": 10 })),
        Some(IngestProgressProfile::ExtractionHeavy)
    );
    assert_eq!(
        progress_profile_from_stage_details(&json!({ "pageCount": 1 })),
        Some(IngestProgressProfile::ExtractionHeavy)
    );
    assert_eq!(progress_profile_from_stage_details(&json!({})), None);
}

#[test]
fn carries_progress_profile_forward_from_prior_stage_events() {
    let existing_events = vec![
        stage_event(INGEST_STAGE_EXTRACT_CONTENT, json!({ "fileKind": "text_like" })),
        stage_event(INGEST_STAGE_CHUNK_CONTENT, json!({ "chunkCount": 4 })),
    ];

    assert_eq!(
        ingest_progress_profile(&existing_events, &json!({})),
        IngestProgressProfile::InlineText
    );
    assert_eq!(
        ingest_attempt_stage_unit_progress_percent(
            &existing_events,
            INGEST_STAGE_EMBED_CHUNK,
            1,
            2,
            &json!({})
        ),
        Some(40)
    );
}

#[test]
fn exposes_content_mutation_stage_unit_progress() {
    assert_eq!(
        ingest_stage_unit_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            0,
            100
        ),
        Some(5)
    );
    assert_eq!(
        ingest_stage_unit_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            50,
            100
        ),
        Some(17)
    );
    assert_eq!(
        ingest_stage_unit_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            100,
            100
        ),
        Some(30)
    );
    assert_eq!(
        ingest_stage_unit_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            101,
            100
        ),
        Some(30)
    );
    assert_eq!(
        ingest_stage_unit_progress_percent_for_profile(
            IngestProgressProfile::ExtractionHeavy,
            INGEST_STAGE_EXTRACT_CONTENT,
            1,
            0
        ),
        None
    );
}

// BUG A (a): a transient/retryable stage failure that still has attempt
// budget left is requeued, NOT terminally failed.
#[test]
fn retryable_failure_under_budget_requeues_instead_of_failing() {
    // First attempt fails retryably -> budget is not exhausted -> requeue.
    assert!(!is_retry_budget_exhausted("failed", true, 1, false));
    let effective_retryable = !is_retry_budget_exhausted("failed", true, 1, false);
    assert!(effective_retryable);
    assert_eq!(
        next_job_queue_state_after_finalize("failed", effective_retryable),
        "queued",
        "a retryable failure with budget left must go back to the queue"
    );
    // A non-retryable failure is terminal regardless of budget.
    assert!(!is_retry_budget_exhausted("failed", false, 1, false));
    assert_eq!(next_job_queue_state_after_finalize("failed", false), "failed");
    // A success is never subject to the retry budget.
    assert!(!is_retry_budget_exhausted("succeeded", true, 99, false));
    assert_eq!(next_job_queue_state_after_finalize("succeeded", true), "completed");
}

#[test]
fn web_discovery_finalization_leaves_the_run_operation_to_exact_page_settlement() {
    assert!(!attempt_finalization_owns_linked_operation("web_discovery"));
    for job_kind in ["content_mutation", "graph_refresh", "web_materialize_page"] {
        assert!(
            attempt_finalization_owns_linked_operation(job_kind),
            "{job_kind} must keep its existing linked-operation finalization",
        );
    }
}

// BUG A (b): once the attempt budget is exhausted, a retryable failure is
// escalated to a terminal `failed`.
#[test]
fn retryable_failure_at_budget_limit_becomes_terminal() {
    // Below the limit -> still retryable.
    assert!(!is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS - 1, false));
    // At and beyond the limit -> exhausted.
    assert!(is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS, false));
    assert!(is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS + 1, false));
    let effective_retryable =
        !is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS, false);
    assert!(!effective_retryable);
    assert_eq!(
        next_job_queue_state_after_finalize("failed", effective_retryable),
        "failed",
        "an exhausted retry budget must finalize the job terminally"
    );
}

// Backoff grows exponentially with the attempt number and is bounded, so the
// requeue delay rides out a multi-minute provider outage without pushing a
// job arbitrarily far into the future.
#[test]
fn retry_backoff_is_exponential_and_bounded() {
    let first = retry_backoff_after_attempt(1);
    let second = retry_backoff_after_attempt(2);
    let third = retry_backoff_after_attempt(3);
    assert!(first.num_seconds() > 0);
    assert_eq!(second.num_seconds(), first.num_seconds() * 2);
    assert_eq!(third.num_seconds(), first.num_seconds() * 4);
    // A very large attempt number saturates at the cap rather than
    // overflowing or producing an unbounded delay.
    let capped = retry_backoff_after_attempt(1000);
    assert!(capped.num_seconds() >= third.num_seconds());
    assert!(capped.num_seconds() <= 600);
}

// An operational deferral (a provider 429 cooldown) is exempt from the attempt
// ceiling: the job is parked, never permanently failed, however many attempts
// it has already burned.
#[test]
fn operational_deferral_bypasses_the_attempt_ceiling() {
    assert!(
        !is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS, true),
        "a deferred job must not be treated as budget-exhausted"
    );
    assert!(
        is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS, false),
        "without a deferral the same attempt count is terminal"
    );
}

// The deferral is a floor, and the ordinary backoff is the fallback: whichever
// is longer wins, so a short `Retry-After` never pulls a job in earlier than its
// exponential backoff and a long quota cooldown is always honored.
#[test]
fn retry_backoff_with_deferral_uses_the_longer_of_backoff_and_deferral() {
    let backoff = retry_backoff_after_attempt(2);
    let shorter = chrono::Duration::seconds(1);
    let longer = backoff + chrono::Duration::minutes(10);

    assert_eq!(retry_backoff_with_deferral(2, None), backoff);
    assert_eq!(retry_backoff_with_deferral(2, Some(shorter)), backoff);
    assert_eq!(retry_backoff_with_deferral(2, Some(longer)), longer);
}
