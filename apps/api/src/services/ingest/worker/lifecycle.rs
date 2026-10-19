use anyhow::Context;
use chrono::Utc;
use tracing::{info, warn};

use crate::{
    infra::repositories::ingest_repository,
    interfaces::http::router_support::ApiError,
    services::{
        ingest::service::{
            FinalizeAttemptCommand, INGEST_STAGE_FINALIZING, INGEST_STAGE_WEB_DISCOVERY,
            INGEST_STAGE_WEB_MATERIALIZE_PAGE, INGEST_STAGE_WEBHOOK_DELIVERY,
        },
        query::vector_dimensions::invalidate_library_embedding_profile_inventory,
        webhook::error::WebhookServiceError,
    },
};

use super::{
    attempt::IngestAttemptLifecycle,
    errors::{
        ContentExtractionError, ContentPipelineFailure, DeletedDocumentJobSkipped,
        JobCanceledByRequest, JobCanceledByShutdown, JobLeaseLost, JobPausedByOperator,
        classify_content_ingest_failure,
    },
};

pub(super) async fn finalize_successful_ingest_job(
    lifecycle: &IngestAttemptLifecycle<'_>,
) -> anyhow::Result<()> {
    let final_stage = match lifecycle.job.job_kind.as_str() {
        "content_mutation" | "graph_refresh" => INGEST_STAGE_FINALIZING,
        "web_discovery" => INGEST_STAGE_WEB_DISCOVERY,
        "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE,
        "webhook_delivery" => INGEST_STAGE_WEBHOOK_DELIVERY,
        _ => lifecycle.initial_stage,
    };
    let result = lifecycle
        .state
        .application_services
        .ingest
        .finalize_attempt(
            lifecycle.state,
            FinalizeAttemptCommand {
                attempt_id: lifecycle.attempt_id,
                knowledge_generation_id: None,
                attempt_state: "succeeded".to_string(),
                current_stage: Some(final_stage.to_string()),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                retryable: false,
                operational_defer: None,
            },
        )
        .await;
    let Err(error) = result else {
        log_ingest_completed(lifecycle);
        return Ok(());
    };
    if !matches!(error, ApiError::Conflict(_)) {
        return Err(error).context("failed to finalize ingest attempt as succeeded");
    }
    if !preserve_paused_success(lifecycle).await {
        log_success_after_lease_loss(lifecycle, &error);
    }
    Ok(())
}

fn log_ingest_completed(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, "ingest job completed");
}

async fn preserve_paused_success(lifecycle: &IngestAttemptLifecycle<'_>) -> bool {
    match ingest_repository::get_ingest_job_by_id(
        &lifecycle.state.persistence.postgres,
        lifecycle.job.id,
    )
    .await
    {
        Ok(Some(row)) if row.queue_state == "paused" => {
            abandon_paused_success(lifecycle).await;
            true
        }
        Err(reload_error) => {
            warn!(attempt_id = %lifecycle.attempt_id, ?reload_error,
                "failed to reload ingest job after finalize conflict");
            false
        }
        Ok(_) => false,
    }
}

async fn abandon_paused_success(lifecycle: &IngestAttemptLifecycle<'_>) {
    if let Err(abandon_error) = ingest_repository::abandon_paused_ingest_attempt(
        &lifecycle.state.persistence.postgres,
        lifecycle.attempt_id,
    )
    .await
    {
        warn!(attempt_id = %lifecycle.attempt_id, ?abandon_error,
            "failed to finalize paused ingest attempt after successful pipeline return");
    }
    log_paused_success(lifecycle);
}

fn log_paused_success(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job completed after operator pause; preserving paused queue state");
}

fn log_success_after_lease_loss(lifecycle: &IngestAttemptLifecycle<'_>, error: &ApiError) {
    warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, ?error,
        "ingest job finished after losing its active lease; leaving queue state to the current owner");
}

async fn handle_cooperative_failure(
    lifecycle: &IngestAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> anyhow::Result<bool> {
    if error.downcast_ref::<JobCanceledByRequest>().is_some() {
        log_user_cancellation(lifecycle);
        return Ok(true);
    }
    if error.downcast_ref::<JobPausedByOperator>().is_some() {
        abandon_operator_paused_attempt(lifecycle).await;
        return Ok(true);
    }
    if error.downcast_ref::<JobLeaseLost>().is_some() {
        log_attempt_lease_loss(lifecycle);
        return Ok(true);
    }
    Ok(false)
}

fn log_user_cancellation(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job observed user cancel request and stopped cooperatively");
}

async fn abandon_operator_paused_attempt(lifecycle: &IngestAttemptLifecycle<'_>) {
    if let Err(abandon_error) = ingest_repository::abandon_paused_ingest_attempt(
        &lifecycle.state.persistence.postgres,
        lifecycle.attempt_id,
    )
    .await
    {
        warn!(attempt_id = %lifecycle.attempt_id, ?abandon_error, "failed to finalize paused ingest attempt");
    }
    log_operator_pause(lifecycle);
}

fn log_operator_pause(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job observed operator pause request and stopped cooperatively");
}

fn log_attempt_lease_loss(lifecycle: &IngestAttemptLifecycle<'_>) {
    warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job stopped because its attempt lease was lost");
}

async fn defer_in_flight_webhook_failure(
    lifecycle: &IngestAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> bool {
    let Some(WebhookServiceError::DeliveryLeaseInFlight { retry_at, .. }) =
        error.downcast_ref::<WebhookServiceError>()
    else {
        return false;
    };
    match ingest_repository::defer_webhook_delivery_in_flight(
        &lifecycle.state.persistence.postgres,
        lifecycle.attempt_id,
        lifecycle.job.id,
        lifecycle.expected_queue_lease_token,
        *retry_at,
    )
    .await
    {
        Ok(true) => log_webhook_deferral(lifecycle, retry_at),
        Ok(false) => log_webhook_deferral_lease_loss(lifecycle),
        Err(defer_error) => {
            warn!(
                worker_id = %lifecycle.worker_id,
                job_id = %lifecycle.job.id,
                attempt_id = %lifecycle.attempt_id,
                ?defer_error,
                "failed to defer duplicate webhook job; using ordinary retry path"
            );
            return false;
        }
    }
    true
}

async fn finalize_special_failure(
    lifecycle: &IngestAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> anyhow::Result<bool> {
    if error.downcast_ref::<JobCanceledByShutdown>().is_some() {
        finalize_shutdown_cancellation(lifecycle).await;
        return Ok(true);
    }
    if error.downcast_ref::<DeletedDocumentJobSkipped>().is_none() {
        return Ok(false);
    }
    finalize_deleted_document(lifecycle).await;
    Ok(true)
}

fn log_webhook_deferral(lifecycle: &IngestAttemptLifecycle<'_>, retry_at: &chrono::DateTime<Utc>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, retry_at = %retry_at,
        "deferred duplicate webhook job until the current delivery lease expires");
}

fn log_webhook_deferral_lease_loss(lifecycle: &IngestAttemptLifecycle<'_>) {
    warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "webhook job lease moved before in-flight delivery deferral");
}

async fn user_cancel_won_shutdown_race(lifecycle: &IngestAttemptLifecycle<'_>) -> bool {
    let current_job = ingest_repository::get_ingest_job_by_id(
        &lifecycle.state.persistence.postgres,
        lifecycle.job.id,
    )
    .await;
    if current_job
        .as_ref()
        .is_ok_and(|row| row.as_ref().is_some_and(|row| row.queue_state == "canceled"))
    {
        log_user_cancel_during_shutdown(lifecycle);
        return true;
    }
    if let Err(reload_error) = current_job {
        warn!(attempt_id = %lifecycle.attempt_id, ?reload_error,
            "failed to reload ingest job while finalizing shutdown cancellation");
    }
    false
}

fn log_user_cancel_during_shutdown(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job stopped during shutdown after user cancel won the race");
}

async fn finalize_shutdown_cancellation(lifecycle: &IngestAttemptLifecycle<'_>) {
    if user_cancel_won_shutdown_race(lifecycle).await {
        return;
    }
    if let Err(finalize_error) = lifecycle
        .state
        .application_services
        .ingest
        .finalize_attempt(
            lifecycle.state,
            FinalizeAttemptCommand {
                attempt_id: lifecycle.attempt_id,
                knowledge_generation_id: None,
                attempt_state: "failed".to_string(),
                current_stage: Some(lifecycle.initial_stage.to_string()),
                failure_class: Some("worker_shutdown".to_string()),
                failure_code: Some("shutdown_cancelled".to_string()),
                failure_message: Some("Worker shutdown canceled document processing".to_string()),
                retryable: true,
                operational_defer: None,
            },
        )
        .await
    {
        warn!(attempt_id = %lifecycle.attempt_id, ?finalize_error, "failed to requeue shutdown-canceled attempt");
    }
    log_shutdown_cancellation(lifecycle);
}

fn log_shutdown_cancellation(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job stopped cooperatively for worker shutdown");
}

async fn finalize_deleted_document(lifecycle: &IngestAttemptLifecycle<'_>) {
    if let Err(finalize_error) = lifecycle
        .state
        .application_services
        .ingest
        .finalize_attempt(
            lifecycle.state,
            FinalizeAttemptCommand {
                attempt_id: lifecycle.attempt_id,
                knowledge_generation_id: None,
                attempt_state: "canceled".to_string(),
                current_stage: Some(lifecycle.initial_stage.to_string()),
                failure_class: Some("content_mutation".to_string()),
                failure_code: Some("document_deleted".to_string()),
                failure_message: Some(
                    "Document was deleted before processing finished".to_string(),
                ),
                retryable: false,
                operational_defer: None,
            },
        )
        .await
    {
        warn!(attempt_id = %lifecycle.attempt_id, ?finalize_error,
            "failed to finalize deleted-document attempt as canceled");
    }
    log_deleted_document(lifecycle);
}

fn log_deleted_document(lifecycle: &IngestAttemptLifecycle<'_>) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "ingest job canceled because document was deleted");
}

async fn publish_content_ingest_failure(
    lifecycle: &IngestAttemptLifecycle<'_>,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    let job = lifecycle.job;
    let (Some(mutation_id), Some(mutation_item_id), Some(document_id), Some(revision_id)) = (
        job.mutation_id,
        job.mutation_item_id,
        job.knowledge_document_id,
        job.knowledge_revision_id,
    ) else {
        warn!(
            worker_id = %lifecycle.worker_id,
            job_id = %job.id,
            attempt_id = %lifecycle.attempt_id,
            "content ingest job has an incomplete lifecycle identity; refusing partial failure publication",
        );
        return Err(error)
            .context("content ingest job has incomplete mutation/document/revision identity");
    };
    let message = format!("{error:#}");
    let classification = classify_content_ingest_failure(&error);
    let outcome = ingest_repository::fail_content_ingest_attempt(
        &lifecycle.state.persistence.postgres,
        &ingest_repository::FailContentIngestAttempt {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            mutation_id,
            mutation_item_id,
            attempt_id: lifecycle.attempt_id,
            current_stage: None,
            failure_class: Some(classification.failure_class.to_string()),
            failure_code: Some(classification.failure_code.clone()),
            failure_disposition: Some(classification.failure_disposition),
            failure_message: Some(message.clone()),
            retryable: classification.is_retryable(),
            operational_defer: classification.operational_defer,
            delete_vectors: error
                .downcast_ref::<ContentPipelineFailure>()
                .is_some_and(|failure| failure.delete_vectors),
            failed_at: Utc::now(),
        },
    )
    .await;
    match outcome {
        Ok(ingest_repository::FailContentIngestAttemptOutcome::Applied {
            deleted,
            retry_scheduled,
            mutation_failed,
            ..
        }) => {
            invalidate_library_embedding_profile_inventory(job.library_id);
            if !retry_scheduled {
                settle_terminal_content_failure(lifecycle, mutation_item_id).await;
            }
            log_content_failure_applied(lifecycle, deleted, retry_scheduled, mutation_failed);
            crate::observability::metrics::record_ingest_attempt_outcome(
                &job.job_kind,
                if retry_scheduled {
                    crate::observability::metrics::IngestOutcome::Requeued
                } else {
                    crate::observability::metrics::IngestOutcome::Failed
                },
                Some(classification.failure_code.as_str()),
            );
            Ok(())
        }
        Ok(ingest_repository::FailContentIngestAttemptOutcome::AuthorityLost { .. }) => {
            log_content_failure_authority_loss(lifecycle);
            Ok(())
        }
        Err(publication_error) => {
            log_content_failure_publication_error(lifecycle, &publication_error);
            Err(error).with_context(|| {
                format!(
                    "atomic content-ingest failure publication also failed: {publication_error:#}; original failure: {message}"
                )
            })
        }
    }
}

async fn settle_terminal_content_failure(
    lifecycle: &IngestAttemptLifecycle<'_>,
    mutation_item_id: uuid::Uuid,
) {
    if let Err(settlement_error) = lifecycle
        .state
        .application_services
        .web_ingest
        .settle_materialized_page_for_mutation_item(lifecycle.state, mutation_item_id)
        .await
    {
        warn!(job_id = %lifecycle.job.id, attempt_id = %lifecycle.attempt_id,
            %mutation_item_id, ?settlement_error,
            "terminal content failure committed but web page settlement failed; read-side reconciliation will retry");
    }
}

fn log_content_failure_applied(
    lifecycle: &IngestAttemptLifecycle<'_>,
    deleted: u64,
    retry_scheduled: bool,
    mutation_failed: bool,
) {
    if retry_scheduled {
        log_content_failure_requeued(lifecycle, deleted);
    } else {
        warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id, deleted, mutation_failed,
            "content ingest was atomically finalized as failed");
    }
}

fn log_content_failure_requeued(lifecycle: &IngestAttemptLifecycle<'_>, deleted: u64) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, deleted,
        "content ingest failed retryably and was atomically requeued");
}

fn log_content_failure_authority_loss(lifecycle: &IngestAttemptLifecycle<'_>) {
    warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "content ingest failure arrived after attempt authority moved; preserving current owner state");
}

fn log_content_failure_publication_error(
    lifecycle: &IngestAttemptLifecycle<'_>,
    publication_error: &impl std::fmt::Debug,
) {
    warn!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, ?publication_error,
        "failed to atomically publish content ingest failure");
}

pub(super) async fn finalize_failed_ingest_job(
    lifecycle: &IngestAttemptLifecycle<'_>,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    if handle_cooperative_failure(lifecycle, &error).await?
        || defer_in_flight_webhook_failure(lifecycle, &error).await
        || finalize_special_failure(lifecycle, &error).await?
    {
        return Ok(());
    }
    if lifecycle.job.job_kind == "content_mutation" {
        return publish_content_ingest_failure(lifecycle, error).await;
    }
    let state = lifecycle.state;
    let job = lifecycle.job;
    let attempt_id = lifecycle.attempt_id;
    let initial_stage = lifecycle.initial_stage.to_string();
    let message = format!("{error:#}");
    let extract_error = error.downcast_ref::<ContentExtractionError>();
    let classification = classify_content_ingest_failure(&error);
    let operational_defer = classification.operational_defer;
    // An operational deferral (a provider 429 on the graph_refresh path) is
    // classified by the error, not by the job kind: surface the provider code so
    // the operator sees `provider_rate_limited` rather than the generic
    // `graph_refresh_failed`. Ordinary failures keep their existing codes.
    let (failure_class, failure_code) =
        ordinary_failure_metadata(&job.job_kind, extract_error, &classification);
    match state
        .application_services
        .ingest
        .finalize_attempt(
            state,
            FinalizeAttemptCommand {
                attempt_id,
                knowledge_generation_id: None,
                attempt_state: "failed".to_string(),
                current_stage: Some(initial_stage.clone()),
                failure_class: Some(failure_class),
                failure_code: Some(failure_code),
                failure_message: Some(message.clone()),
                retryable: classification.is_retryable(),
                operational_defer,
            },
        )
        .await
    {
        Ok(finalized) if finalized.retryable => {
            // The finalizer already requeued the job; do not let the dispatcher
            // overwrite it with terminal failure while the backoff runs.
            log_retryable_ingest_failure(lifecycle, finalized.attempt_number);
            Ok(())
        }
        Ok(_) => Err(error).context(message),
        Err(e) => {
            tracing::warn!(%attempt_id, ?e, "failed to finalize attempt as failed");
            Err(error).context(message)
        }
    }
}

fn log_retryable_ingest_failure(lifecycle: &IngestAttemptLifecycle<'_>, attempt_number: i32) {
    info!(worker_id = %lifecycle.worker_id, job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id, attempt_number,
        "ingest job failed retryably; requeued for another attempt");
}

fn ordinary_failure_metadata(
    job_kind: &str,
    extract_error: Option<&ContentExtractionError>,
    classification: &super::errors::ContentIngestFailureClassification,
) -> (String, String) {
    if classification.operational_defer.is_some() {
        (classification.failure_class.to_string(), classification.failure_code.clone())
    } else {
        (
            match job_kind {
                "content_mutation" if extract_error.is_some() => "content_extract",
                "web_discovery" => "web_discovery",
                "web_materialize_page" => "web_page_materialization",
                "graph_refresh" => "graph_refresh",
                _ => "worker_error",
            }
            .to_string(),
            extract_error.map_or_else(
                || match job_kind {
                    "web_discovery" => "web_discovery_failed".to_string(),
                    "web_materialize_page" => "web_materialize_page_failed".to_string(),
                    "graph_refresh" => "graph_refresh_failed".to_string(),
                    _ => "\u{63}anonical_pipeline_failed".to_string(),
                },
                |failure| failure.failure_code.clone(),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_failure_metadata_retains_job_codes_and_extraction_override() {
        let classification = classify_content_ingest_failure(&anyhow::anyhow!("failure"));
        for (kind, class, code) in [
            ("web_discovery", "web_discovery", "web_discovery_failed"),
            ("web_materialize_page", "web_page_materialization", "web_materialize_page_failed"),
            ("graph_refresh", "graph_refresh", "graph_refresh_failed"),
            ("other", "worker_error", "\u{63}anonical_pipeline_failed"),
        ] {
            assert_eq!(
                ordinary_failure_metadata(kind, None, &classification),
                (class.to_string(), code.to_string())
            );
        }
        let extract_error =
            ContentExtractionError::extraction_failed("extract_failed", "extraction failed");
        assert_eq!(
            ordinary_failure_metadata("content_mutation", Some(&extract_error), &classification),
            ("content_extract".to_string(), "extract_failed".to_string()),
        );
        assert_eq!(
            ordinary_failure_metadata("graph_refresh", Some(&extract_error), &classification),
            ("graph_refresh".to_string(), "extract_failed".to_string()),
        );
    }

    #[test]
    fn operational_deferral_keeps_provider_classification_instead_of_job_defaults() {
        let mut classification = classify_content_ingest_failure(&anyhow::anyhow!("failure"));
        classification.failure_class = "provider";
        classification.failure_code = "provider_rate_limited".to_string();
        classification.operational_defer = Some(chrono::Duration::seconds(60));
        let extract_error =
            ContentExtractionError::extraction_failed("extract_failed", "extraction failed");
        assert_eq!(
            ordinary_failure_metadata("graph_refresh", Some(&extract_error), &classification),
            ("provider".to_string(), "provider_rate_limited".to_string()),
        );
    }
}
