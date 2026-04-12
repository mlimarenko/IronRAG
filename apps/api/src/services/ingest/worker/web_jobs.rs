use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ingest_repository,
    services::ingest::service::{
        INGEST_STAGE_WEB_DISCOVERY, INGEST_STAGE_WEB_MATERIALIZE_PAGE, RecordStageEventCommand,
    },
};

pub(super) async fn run_canonical_web_discovery_job(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
) -> anyhow::Result<()> {
    let run_id = resolve_canonical_job_subject_id(state, job, "content_web_ingest_run").await?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                stage_state: "started".to_string(),
                message: Some("discovering recursive crawl scope".to_string()),
                details_json: serde_json::json!({ "runId": run_id }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
            },
        )
        .await
        .context("failed to record web_discovery start stage event")?;
    match state.canonical_services.web_ingest.execute_recursive_discovery_job(state, run_id).await {
        Ok(()) => {
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                        stage_state: "completed".to_string(),
                        message: Some(
                            "recursive crawl scope closed and page jobs queued".to_string(),
                        ),
                        details_json: serde_json::json!({ "runId": run_id }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms: None,
                    },
                )
                .await
                .context("failed to record web_discovery stage event")?;
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("recursive crawl discovery failed".to_string()),
                        details_json: serde_json::json!({
                            "runId": run_id,
                            "error": error_message,
                        }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms: None,
                    },
                )
                .await
                .context("failed to record web_discovery failure stage event")?;
            Err(anyhow::anyhow!("web discovery job failed: {}", error))
        }
    }
}

pub(super) async fn run_canonical_web_materialize_page_job(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
) -> anyhow::Result<()> {
    let candidate_id =
        resolve_canonical_job_subject_id(state, job, "content_web_discovered_page").await?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                stage_state: "started".to_string(),
                message: Some("materializing discovered page from stored snapshot".to_string()),
                details_json: serde_json::json!({ "candidateId": candidate_id }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
            },
        )
        .await
        .context("failed to record web_materialize_page start stage event")?;
    match state.canonical_services.web_ingest.execute_recursive_page_job(state, candidate_id).await
    {
        Ok(()) => {
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                        stage_state: "completed".to_string(),
                        message: Some("discovered page materialized".to_string()),
                        details_json: serde_json::json!({ "candidateId": candidate_id }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms: None,
                    },
                )
                .await
                .context("failed to record web_materialize_page stage event")?;
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("discovered page materialization failed".to_string()),
                        details_json: serde_json::json!({
                            "candidateId": candidate_id,
                            "error": error_message,
                        }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms: None,
                    },
                )
                .await
                .context("failed to record web_materialize_page failure stage event")?;
            Err(anyhow::anyhow!("web page materialization job failed: {}", error))
        }
    }
}

pub(super) async fn resolve_canonical_job_subject_id(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    expected_subject_kind: &str,
) -> anyhow::Result<Uuid> {
    let operation_id =
        job.async_operation_id.context("canonical web ingest job is missing async_operation_id")?;
    let operation = state
        .canonical_services
        .ops
        .get_async_operation(state, operation_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let subject_kind = operation
        .subject_kind
        .as_deref()
        .context("canonical web ingest job subject_kind is missing")?;
    let subject_id =
        operation.subject_id.context("canonical web ingest job subject_id is missing")?;
    if subject_kind != expected_subject_kind {
        anyhow::bail!(
            "canonical web ingest job subject kind mismatch: expected {}, found {}",
            expected_subject_kind,
            subject_kind
        );
    }
    Ok(subject_id)
}
