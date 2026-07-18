use chrono::Utc;
use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ingest_repository::{self, IngestJobRow},
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
    let Some(existing) = load_canonical_job_for_failure(state, job_id, worker_id).await else {
        return;
    };
    if canonical_job_failure_must_be_deferred(&existing, worker_id) {
        log_deferred_exact_content_failure(&existing, worker_id, job_id, &message);
        return;
    }

    mark_canonical_job_failed(state, &existing, job_id, worker_id, &message).await;
    let failure_code = canonical_job_failure_code(state, &existing, job_id).await;
    reconcile_canonical_job_failure(state, &existing, job_id, worker_id, &failure_code, &message)
        .await;
}

async fn load_canonical_job_for_failure(
    state: &AppState,
    job_id: Uuid,
    worker_id: &str,
) -> Option<IngestJobRow> {
    match ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id).await {
        Ok(Some(row)) => Some(row),
        Ok(None) => {
            error!(%worker_id, %job_id, "canonical ingest job vanished while trying to fail it");
            None
        }
        Err(db_error) => {
            error!(%worker_id, %job_id, ?db_error, "failed to load canonical ingest job for failure");
            None
        }
    }
}

fn canonical_job_failure_must_be_deferred(existing: &IngestJobRow, worker_id: &str) -> bool {
    matches!(existing.queue_state.as_str(), "completed" | "canceled" | "queued")
        || exact_content_failure_requires_recovery(existing)
        || leased_by_another_worker(existing, worker_id)
}

fn exact_content_failure_requires_recovery(existing: &IngestJobRow) -> bool {
    existing.job_kind == "content_mutation" && existing.mutation_item_id.is_some()
}

fn leased_by_another_worker(existing: &IngestJobRow, worker_id: &str) -> bool {
    existing.queue_state == "leased"
        && existing.queue_lease_owner.as_deref().is_some_and(|owner| owner != worker_id)
}

fn log_deferred_exact_content_failure(
    existing: &IngestJobRow,
    worker_id: &str,
    job_id: Uuid,
    message: &str,
) {
    if !exact_content_failure_requires_recovery(existing) {
        return;
    }
    error!(
        %worker_id,
        %job_id,
        queue_state = %existing.queue_state,
        original_error = %message,
        "exact content ingest failure was left to atomic lifecycle recovery",
    );
}

async fn mark_canonical_job_failed(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
    worker_id: &str,
    message: &str,
) {
    if existing.queue_state == "failed" {
        return;
    }
    let update = ingest_repository::UpdateIngestJob {
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
    };
    if let Err(db_error) =
        ingest_repository::update_ingest_job(&state.persistence.postgres, job_id, &update).await
    {
        error!(
            %worker_id,
            %job_id,
            ?db_error,
            original_error = %message,
            "failed to mark canonical ingest job as failed",
        );
    }
}

async fn canonical_job_failure_code(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
) -> String {
    latest_canonical_attempt_failure_code(state, job_id)
        .await
        .unwrap_or_else(|| default_canonical_job_failure_code(&existing.job_kind).to_string())
}

fn default_canonical_job_failure_code(job_kind: &str) -> &'static str {
    match job_kind {
        "web_discovery" => "web_discovery_failed",
        "web_materialize_page" => "web_materialize_page_failed",
        _ => "canonical_pipeline_failed",
    }
}

async fn reconcile_canonical_job_failure(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
    worker_id: &str,
    failure_code: &str,
    message: &str,
) {
    match existing.job_kind.as_str() {
        "web_discovery" => {
            reconcile_recursive_discovery_failure(
                state,
                existing,
                job_id,
                worker_id,
                failure_code,
                message,
            )
            .await;
        }
        "web_materialize_page" => {
            reconcile_recursive_page_failure(
                state,
                existing,
                job_id,
                worker_id,
                failure_code,
                message,
            )
            .await;
        }
        _ => {
            reconcile_content_mutation_failure(
                state,
                existing,
                job_id,
                worker_id,
                failure_code,
                message,
            )
            .await;
        }
    }
}

async fn reconcile_recursive_discovery_failure(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
    worker_id: &str,
    failure_code: &str,
    message: &str,
) {
    let run_id =
        match resolve_canonical_job_subject_id(state, existing, "content_web_ingest_run").await {
            Ok(run_id) => run_id,
            Err(resolve_error) => {
                error!(
                    %worker_id,
                    %job_id,
                    ?resolve_error,
                    original_error = %message,
                    "failed to resolve recursive discovery run subject",
                );
                return;
            }
        };
    if let Err(reconcile_error) = state
        .canonical_services
        .web_ingest
        .fail_recursive_discovery_job(state, run_id, failure_code)
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

async fn reconcile_recursive_page_failure(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
    worker_id: &str,
    failure_code: &str,
    message: &str,
) {
    let candidate_id = match resolve_canonical_job_subject_id(
        state,
        existing,
        "content_web_discovered_page",
    )
    .await
    {
        Ok(candidate_id) => candidate_id,
        Err(resolve_error) => {
            error!(
                %worker_id,
                %job_id,
                ?resolve_error,
                original_error = %message,
                "failed to resolve recursive page subject",
            );
            return;
        }
    };
    if let Err(reconcile_error) = state
        .canonical_services
        .web_ingest
        .fail_recursive_page_job(state, candidate_id, failure_code)
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

async fn reconcile_content_mutation_failure(
    state: &AppState,
    existing: &IngestJobRow,
    job_id: Uuid,
    worker_id: &str,
    failure_code: &str,
    message: &str,
) {
    let Some(mutation_id) = existing.mutation_id else {
        return;
    };
    let command = ReconcileFailedIngestMutationCommand {
        mutation_id,
        failure_code: failure_code.to_string(),
        failure_message: message.to_string(),
    };
    if let Err(reconcile_error) =
        state.canonical_services.content.reconcile_failed_ingest_mutation(state, command).await
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
