use chrono::Utc;
use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::ingest_repository,
    services::content::service::ReconcileFailedIngestMutationCommand,
};

use super::web_jobs::resolve_canonical_job_subject_id;

async fn latest_canonical_attempt_failure_code(state: &AppState, job_id: Uuid) -> Option<String> {
    ingest_repository::get_latest_ingest_attempt_by_job(&state.persistence.postgres, job_id)
        .await
        .ok()
        .flatten()
        .and_then(|attempt| attempt.failure_code)
}

pub(super) async fn fail_canonical_ingest_job(
    state: &AppState,
    job_id: Uuid,
    worker_id: &str,
    error: &anyhow::Error,
) {
    let message = format!("{error:#}");
    let existing = match ingest_repository::get_ingest_job_by_id(
        &state.persistence.postgres,
        job_id,
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            error!(%worker_id, %job_id, "canonical ingest job vanished while trying to fail it");
            return;
        }
        Err(db_error) => {
            error!(%worker_id, %job_id, ?db_error, "failed to load canonical ingest job for failure");
            return;
        }
    };

    if existing.queue_state == "completed" || existing.queue_state == "canceled" {
        // A canceled job has already reached its terminal state via the cancel
        // endpoint; demoting it to "failed" here would clobber user intent.
        return;
    }

    if existing.queue_state != "failed" {
        let update_result = ingest_repository::update_ingest_job(
            &state.persistence.postgres,
            job_id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: existing.mutation_id,
                connector_id: existing.connector_id,
                async_operation_id: existing.async_operation_id,
                knowledge_document_id: existing.knowledge_document_id,
                knowledge_revision_id: existing.knowledge_revision_id,
                job_kind: existing.job_kind.clone(),
                queue_state: "failed".to_string(),
                priority: existing.priority,
                dedupe_key: existing.dedupe_key.clone(),
                available_at: existing.available_at,
                completed_at: Some(Utc::now()),
            },
        )
        .await;
        if let Err(db_error) = update_result {
            error!(
                %worker_id,
                %job_id,
                ?db_error,
                original_error = %message,
                "failed to mark canonical ingest job as failed",
            );
        }
    }

    let failure_code = latest_canonical_attempt_failure_code(state, job_id).await.unwrap_or_else(
        || match existing.job_kind.as_str() {
            "web_discovery" => "web_discovery_failed".to_string(),
            "web_materialize_page" => "web_materialize_page_failed".to_string(),
            _ => "canonical_pipeline_failed".to_string(),
        },
    );
    if existing.job_kind == "web_discovery" {
        match resolve_canonical_job_subject_id(state, &existing, "content_web_ingest_run").await {
            Ok(run_id) => {
                if let Err(reconcile_error) = state
                    .canonical_services
                    .web_ingest
                    .fail_recursive_discovery_job(state, run_id, &failure_code)
                    .await
                {
                    error!(
                        %worker_id,
                        %job_id,
                        %run_id,
                        ?reconcile_error,
                        original_error = %message,
                        "failed to reconcile recursive discovery job failure",
                    );
                }
            }
            Err(resolve_error) => {
                error!(
                    %worker_id,
                    %job_id,
                    ?resolve_error,
                    original_error = %message,
                    "failed to resolve recursive discovery run subject",
                );
            }
        }
        return;
    }
    if existing.job_kind == "web_materialize_page" {
        match resolve_canonical_job_subject_id(state, &existing, "content_web_discovered_page")
            .await
        {
            Ok(candidate_id) => {
                if let Err(reconcile_error) = state
                    .canonical_services
                    .web_ingest
                    .fail_recursive_page_job(state, candidate_id, &failure_code)
                    .await
                {
                    error!(
                        %worker_id,
                        %job_id,
                        %candidate_id,
                        ?reconcile_error,
                        original_error = %message,
                        "failed to reconcile recursive page job failure",
                    );
                }
            }
            Err(resolve_error) => {
                error!(
                    %worker_id,
                    %job_id,
                    ?resolve_error,
                    original_error = %message,
                    "failed to resolve recursive page subject",
                );
            }
        }
        return;
    }
    if let Some(mutation_id) = existing.mutation_id
        && let Err(reconcile_error) = state
            .canonical_services
            .content
            .reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id,
                    failure_code,
                    failure_message: message.clone(),
                },
            )
            .await
    {
        error!(
            %worker_id,
            %job_id,
            ?reconcile_error,
            original_error = %message,
            "failed to reconcile canonical content mutation after ingest failure",
        );
    }
}
