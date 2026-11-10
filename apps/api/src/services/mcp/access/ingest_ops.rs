//! Authorization and mapping for the ingest diagnostics MCP tools.
//!
//! Every function here resolves the caller's permission against the concrete
//! library before touching ingest state. This module is the only place the
//! connector read/write policies are applied to MCP ingest calls; the tool
//! handlers in `interfaces::http::mcp::tools::ingest` only parse arguments.

use crate::{
    app::state::AppState,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    infra::repositories::ingest_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_CONNECTOR_READ, POLICY_CONNECTOR_WRITE, authorize_library_permission,
        },
        ingest_pagination::{
            IngestJobListCursor, decode_page_cursor, encode_page_cursor,
            parse_ingest_queue_state_filter,
        },
        router_support::ApiError,
    },
    mcp_types::{
        McpCancelIngestJobRequest, McpGetIngestJobRequest, McpIngestAttempt, McpIngestJobDetail,
        McpIngestJobList, McpIngestJobSummary, McpIngestStageEvent, McpListIngestJobsRequest,
        McpRetryIngestJobRequest,
    },
};
use uuid::Uuid;

use super::catalog::load_library_by_catalog_ref;

const DEFAULT_JOB_PAGE_LIMIT: u32 = 50;
const MAX_JOB_PAGE_LIMIT: u32 = 200;
const DEFAULT_ATTEMPT_LIMIT: u32 = 10;
const MAX_ATTEMPT_LIMIT: u32 = 20;

pub(crate) async fn list_ingest_jobs(
    auth: &AuthContext,
    state: &AppState,
    request: McpListIngestJobsRequest,
) -> Result<McpIngestJobList, ApiError> {
    let library =
        load_library_by_catalog_ref(auth, state, &request.library, POLICY_CONNECTOR_READ).await?;
    let status_filter = normalize_queue_state_filter(request.queue_states.as_deref())?;
    let limit = job_page_limit(request.limit);
    let cursor = match request.cursor.as_deref() {
        Some(token) => {
            let decoded: IngestJobListCursor = decode_page_cursor(token)?;
            Some((decoded.queued_at, decoded.job_id))
        }
        None => None,
    };

    let (handles, has_more) = state
        .application_services
        .ingest
        .list_job_handles_page(state, library.id, cursor, i64::from(limit), &status_filter)
        .await?;

    // Same keyset and page order as the REST list: the cursor is the last
    // returned row's `(queued_at, job_id)`.
    let next_cursor = if has_more {
        handles.last().map(|handle| {
            encode_page_cursor(&IngestJobListCursor {
                queued_at: handle.job.queued_at,
                job_id: handle.job.id,
            })
        })
    } else {
        None
    };

    Ok(McpIngestJobList {
        items: handles
            .iter()
            .map(|handle| map_ingest_job_summary(&handle.job, handle.latest_attempt.as_ref()))
            .collect(),
        next_cursor,
    })
}

pub(crate) async fn get_ingest_job(
    auth: &AuthContext,
    state: &AppState,
    request: McpGetIngestJobRequest,
) -> Result<McpIngestJobDetail, ApiError> {
    let handle = state.application_services.ingest.get_job_handle(state, request.job_id).await?;
    authorize_library_permission(
        auth,
        handle.job.workspace_id,
        handle.job.library_id,
        POLICY_CONNECTOR_READ,
    )?;

    let attempt_limit = attempt_page_limit(request.attempt_limit);
    let (attempts, has_more_attempts) = state
        .application_services
        .ingest
        .list_attempts_page(state, request.job_id, None, i64::from(attempt_limit))
        .await?;
    // `list_attempts_page` orders by `attempt_number desc, id desc`, so the
    // first row is the latest attempt.
    let stage_events = match attempts.first() {
        Some(attempt) => {
            state.application_services.ingest.list_stage_events(state, attempt.id).await?
        }
        None => Vec::new(),
    };

    Ok(McpIngestJobDetail {
        job: map_ingest_job_summary(&handle.job, handle.latest_attempt.as_ref()),
        attempts: attempts.iter().map(map_ingest_attempt).collect(),
        has_more_attempts,
        stage_events: stage_events.iter().map(map_stage_event).collect(),
    })
}

pub(crate) async fn retry_ingest_job(
    auth: &AuthContext,
    state: &AppState,
    request: McpRetryIngestJobRequest,
) -> Result<McpIngestJobSummary, ApiError> {
    let job = state.application_services.ingest.get_job(state, request.job_id).await?;
    authorize_library_permission(auth, job.workspace_id, job.library_id, POLICY_CONNECTOR_WRITE)?;
    state.application_services.ingest.retry_job(state, request.job_id, None).await?;
    read_job_summary(state, request.job_id).await
}

pub(crate) async fn cancel_ingest_job(
    auth: &AuthContext,
    state: &AppState,
    request: McpCancelIngestJobRequest,
) -> Result<McpIngestJobSummary, ApiError> {
    let job = state.application_services.ingest.get_job(state, request.job_id).await?;
    authorize_library_permission(auth, job.workspace_id, job.library_id, POLICY_CONNECTOR_WRITE)?;
    let canceled = ingest_repository::cancel_ingest_job(&state.persistence.postgres, job.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if canceled == 0 {
        return Err(ApiError::BadRequest(
            "Only queued or running jobs can be canceled".to_string(),
        ));
    }
    read_job_summary(state, job.id).await
}

/// Re-reads one job after a mutation so the caller sees the stored state, not
/// the state it asked for.
async fn read_job_summary(state: &AppState, job_id: Uuid) -> Result<McpIngestJobSummary, ApiError> {
    let handle = state.application_services.ingest.get_job_handle(state, job_id).await?;
    Ok(map_ingest_job_summary(&handle.job, handle.latest_attempt.as_ref()))
}

/// Validates the MCP `queueStates` array against the shared ingest vocabulary.
///
/// The accepted values and the error text have one owner,
/// [`parse_ingest_queue_state_filter`]; joining the array keeps the MCP and
/// REST filters identical instead of re-listing the states here.
fn normalize_queue_state_filter(queue_states: Option<&[String]>) -> Result<Vec<String>, ApiError> {
    let Some(queue_states) = queue_states else {
        return Ok(Vec::new());
    };
    parse_ingest_queue_state_filter(&queue_states.join(","))
}

fn job_page_limit(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_JOB_PAGE_LIMIT).clamp(1, MAX_JOB_PAGE_LIMIT)
}

fn attempt_page_limit(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_ATTEMPT_LIMIT).clamp(1, MAX_ATTEMPT_LIMIT)
}

fn map_ingest_job_summary(
    job: &IngestJob,
    latest_attempt: Option<&IngestAttempt>,
) -> McpIngestJobSummary {
    McpIngestJobSummary {
        job_id: job.id,
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        job_kind: job.job_kind.clone(),
        queue_state: job.queue_state.clone(),
        priority: job.priority,
        dedupe_key: job.dedupe_key.clone(),
        async_operation_id: job.async_operation_id,
        queued_at: job.queued_at,
        available_at: job.available_at,
        completed_at: job.completed_at,
        latest_attempt: latest_attempt.map(map_ingest_attempt),
    }
}

fn map_ingest_attempt(attempt: &IngestAttempt) -> McpIngestAttempt {
    McpIngestAttempt {
        attempt_id: attempt.id,
        attempt_number: attempt.attempt_number,
        attempt_state: attempt.attempt_state.clone(),
        current_stage: attempt.current_stage.clone(),
        failure_class: attempt.failure_class.clone(),
        failure_code: attempt.failure_code.clone(),
        failure_message: attempt.failure_message.clone(),
        progress_percent: attempt.progress_percent,
        retryable: attempt.retryable,
        started_at: attempt.started_at,
        finished_at: attempt.finished_at,
    }
}

fn map_stage_event(event: &IngestStageEvent) -> McpIngestStageEvent {
    McpIngestStageEvent {
        stage_name: event.stage_name.clone(),
        stage_state: event.stage_state.clone(),
        ordinal: event.ordinal,
        message: event.message.clone(),
        recorded_at: event.recorded_at,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        attempt_page_limit, job_page_limit, map_ingest_job_summary, normalize_queue_state_filter,
    };
    use crate::domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent};

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(seconds)
    }

    fn job() -> IngestJob {
        IngestJob {
            id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            mutation_id: None,
            connector_id: None,
            async_operation_id: Some(Uuid::from_u128(4)),
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "connector_sync".to_string(),
            queue_state: "leased".to_string(),
            priority: 7,
            dedupe_key: Some("bookstack:page:42".to_string()),
            queued_at: at(1_000),
            available_at: at(1_100),
            completed_at: None,
        }
    }

    fn attempt() -> IngestAttempt {
        IngestAttempt {
            id: Uuid::from_u128(5),
            job_id: Uuid::from_u128(1),
            attempt_number: 3,
            worker_principal_id: Some(Uuid::from_u128(6)),
            lease_token: Some("lease-value".to_string()),
            knowledge_generation_id: None,
            attempt_state: "leased".to_string(),
            current_stage: Some("extract".to_string()),
            started_at: at(1_200),
            heartbeat_at: Some(at(1_260)),
            finished_at: None,
            failure_class: Some("transient".to_string()),
            failure_code: Some("source_timeout".to_string()),
            failure_message: Some("upstream timed out".to_string()),
            progress_percent: 35,
            retryable: true,
        }
    }

    fn stage_event() -> IngestStageEvent {
        IngestStageEvent {
            id: Uuid::from_u128(8),
            attempt_id: Uuid::from_u128(5),
            stage_name: "extract".to_string(),
            stage_state: "running".to_string(),
            ordinal: 2,
            message: Some("started".to_string()),
            details_json: json!({}),
            recorded_at: at(1_240),
        }
    }

    #[test]
    fn job_summary_maps_to_camel_case_wire_shape() {
        let attempt = attempt();
        let mapped = map_ingest_job_summary(&job(), Some(&attempt));

        assert_eq!(
            serde_json::to_value(mapped).expect("serializable summary"),
            json!({
                "jobId": "00000000-0000-0000-0000-000000000001",
                "workspaceId": "00000000-0000-0000-0000-000000000002",
                "libraryId": "00000000-0000-0000-0000-000000000003",
                "jobKind": "connector_sync",
                "queueState": "leased",
                "priority": 7,
                "dedupeKey": "bookstack:page:42",
                "asyncOperationId": "00000000-0000-0000-0000-000000000004",
                "queuedAt": at(1_000),
                "availableAt": at(1_100),
                "completedAt": null,
                "latestAttempt": {
                    "attemptId": "00000000-0000-0000-0000-000000000005",
                    "attemptNumber": 3,
                    "attemptState": "leased",
                    "currentStage": "extract",
                    "failureClass": "transient",
                    "failureCode": "source_timeout",
                    "failureMessage": "upstream timed out",
                    "progressPercent": 35,
                    "retryable": true,
                    "startedAt": at(1_200),
                    "finishedAt": null
                }
            })
        );
    }

    #[test]
    fn job_summary_without_attempt_keeps_an_absent_latest_attempt() {
        let mapped = map_ingest_job_summary(&job(), None);

        assert_eq!(
            serde_json::to_value(mapped).expect("serializable summary")["latestAttempt"],
            json!(null)
        );
    }

    #[test]
    fn stage_event_maps_to_camel_case_wire_shape() {
        assert_eq!(
            serde_json::to_value(super::map_stage_event(&stage_event()))
                .expect("serializable stage event"),
            json!({
                "stageName": "extract",
                "stageState": "running",
                "ordinal": 2,
                "message": "started",
                "recordedAt": at(1_240)
            })
        );
    }

    #[test]
    fn page_limits_default_and_clamp() {
        assert_eq!(job_page_limit(None), 50);
        assert_eq!(job_page_limit(Some(0)), 1);
        assert_eq!(job_page_limit(Some(999)), 200);
        assert_eq!(job_page_limit(Some(75)), 75);

        assert_eq!(attempt_page_limit(None), 10);
        assert_eq!(attempt_page_limit(Some(0)), 1);
        assert_eq!(attempt_page_limit(Some(999)), 20);
        assert_eq!(attempt_page_limit(Some(4)), 4);
    }

    #[test]
    fn unknown_queue_state_reports_the_shared_vocabulary() {
        let error = normalize_queue_state_filter(Some(&["bogus".to_string()]))
            .expect_err("unknown state must be rejected");

        let message = match error {
            crate::interfaces::http::router_support::ApiError::BadRequest(message) => message,
            other => format!("unexpected error: {other:?}"),
        };
        assert_eq!(
            message,
            "unknown status filter value `bogus`; allowed: queued, leased, completed, failed, canceled, paused"
        );
    }

    #[test]
    fn queue_state_filter_accepts_known_values_and_blank_input() {
        assert_eq!(normalize_queue_state_filter(None).expect("no filter"), Vec::<String>::new());
        assert_eq!(
            normalize_queue_state_filter(Some(&[])).expect("empty filter"),
            Vec::<String>::new()
        );
        assert_eq!(
            normalize_queue_state_filter(Some(&["queued".to_string(), "paused".to_string()]))
                .expect("known states"),
            vec!["queued".to_string(), "paused".to_string()]
        );
    }
}
