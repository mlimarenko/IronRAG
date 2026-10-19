use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    domains::webhook::WebhookEvent,
    infra::repositories::{
        catalog_repository, runtime_graph_repository, webhook_outbox_repository,
    },
};

use super::super::jobs::UpdateIngestJob;
use super::rows::{IngestAttemptRow, UpdateIngestAttempt};

/// Terminal runtime states for an attempt that just reached `attempt_state`:
/// a canceled attempt cancels its runtime rows, every other terminal attempt
/// fails them.
fn terminal_runtime_states(attempt_state: &str) -> Option<(&'static str, &'static str)> {
    match attempt_state {
        "succeeded" | "failed" => Some(("failed", "failed")),
        "canceled" => Some(("canceled", "canceled")),
        _ => None,
    }
}

pub async fn update_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
    input: &UpdateIngestAttempt,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    write_ingest_attempt_row(postgres, attempt_id, input, AttemptUpdateFence::AnyState).await
}

enum AttemptUpdateFence {
    AnyState,
    Leased,
}

/// Both updates persist the same fields; finalization additionally requires a
/// currently leased attempt. The SQL fragments and bind sequence have one owner.
async fn write_ingest_attempt_row<'e, E>(
    executor: E,
    attempt_id: Uuid,
    input: &UpdateIngestAttempt,
    fence: AttemptUpdateFence,
) -> Result<Option<IngestAttemptRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    macro_rules! attempt_update_sql {
        ($fence:literal) => {
            concat!(
                "update ingest_attempt
                 set worker_principal_id = $2,
                     lease_token = $3,
                     knowledge_generation_id = $4,
                     attempt_state = $5::ingest_attempt_state,
                     current_stage = $6,
                     heartbeat_at = $7,
                     finished_at = $8,
                     failure_class = $9,
                     failure_code = $10,
                     failure_message = $11,
                     progress_percent = $12,
                     retryable = $13
                 where id = $1",
                $fence,
                "
                 returning
                     id,
                     job_id,
                     attempt_number,
                     worker_principal_id,
                     lease_token,
                     knowledge_generation_id,
                     attempt_state::text as attempt_state,
                     current_stage,
                     started_at,
                     heartbeat_at,
                     finished_at,
                     failure_class,
                     failure_code,
                     failure_message,
                     progress_percent,
                     retryable"
            )
        };
    }
    let statement = match fence {
        AttemptUpdateFence::AnyState => attempt_update_sql!(""),
        AttemptUpdateFence::Leased => attempt_update_sql!(" and attempt_state = 'leased'"),
    };
    sqlx::query_as::<_, IngestAttemptRow>(statement)
        .bind(attempt_id)
        .bind(input.worker_principal_id)
        .bind(&input.lease_token)
        .bind(input.knowledge_generation_id)
        .bind(&input.attempt_state)
        .bind(&input.current_stage)
        .bind(input.heartbeat_at)
        .bind(input.finished_at)
        .bind(&input.failure_class)
        .bind(&input.failure_code)
        .bind(&input.failure_message)
        .bind(input.progress_percent)
        .bind(input.retryable)
        .fetch_optional(executor)
        .await
}

pub async fn finalize_leased_ingest_attempt_and_update_job(
    postgres: &PgPool,
    attempt_id: Uuid,
    attempt_input: &UpdateIngestAttempt,
    job_input: &UpdateIngestJob,
    lifecycle_event: Option<&WebhookEvent>,
) -> Result<Option<IngestAttemptRow>, sqlx::Error> {
    let mut tx = postgres.begin().await?;
    if let Some(event) = lifecycle_event {
        let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *tx,
            event.workspace_id,
            event.library_id,
        )
        .await?;
        if !parent_locked {
            tx.rollback().await?;
            return Ok(None);
        }
    }
    // Required queue lock order is parent scope (when present) -> job ->
    // attempt. Read the attempt identity without locking it, then acquire the
    // owning job row before any attempt UPDATE. Delete, cancel, recovery, and
    // webhook handoff paths use the same order.
    let locked_job_id = sqlx::query_scalar::<_, Uuid>(
        "select job.id
         from ingest_attempt attempt
         join ingest_job job on job.id = attempt.job_id
         where attempt.id = $1
           and attempt.attempt_state = 'leased'
           and job.queue_state = 'leased'
         for update of job",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(locked_job_id) = locked_job_id else {
        tx.rollback().await?;
        return Ok(None);
    };
    let finalized_attempt =
        write_ingest_attempt_row(&mut *tx, attempt_id, attempt_input, AttemptUpdateFence::Leased)
            .await?;

    let Some(finalized_attempt) = finalized_attempt else {
        tx.rollback().await?;
        return Ok(None);
    };
    if finalized_attempt.job_id != locked_job_id {
        tx.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "ingest attempt changed jobs while finalizing its queue lease".to_string(),
        ));
    }

    // Close the attempt's graph-extraction runtime rows in the same transaction
    // that terminalizes the attempt. Without this an attempt that reaches a
    // terminal state leaves its runtime_execution `accepted`/`running` and its
    // runtime_graph_extraction `processing` forever.
    if let Some((extraction_status, execution_state)) =
        terminal_runtime_states(&finalized_attempt.attempt_state)
    {
        runtime_graph_repository::terminalize_runtime_rows_for_attempt(
            &mut tx,
            finalized_attempt.id,
            extraction_status,
            execution_state,
            finalized_attempt
                .failure_code
                .as_deref()
                .unwrap_or(runtime_graph_repository::RECONCILED_ORPHAN_FAILURE_CODE),
        )
        .await?;
    }

    let job_finalized =
        complete_finalized_attempt_job(&mut tx, &finalized_attempt, job_input, lifecycle_event)
            .await?;
    if !job_finalized {
        tx.rollback().await?;
        return Ok(None);
    }

    tx.commit().await?;
    Ok(Some(finalized_attempt))
}

/// Mirrors the finalized attempt's terminal state onto its leased queue job and,
/// when a lifecycle event is present, enqueues that event and bumps the
/// library's source-truth version in the same transaction. Returns `false` when
/// the job lease moved (zero rows updated); the caller then rolls back.
async fn complete_finalized_attempt_job(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    finalized_attempt: &IngestAttemptRow,
    job_input: &UpdateIngestJob,
    lifecycle_event: Option<&WebhookEvent>,
) -> Result<bool, sqlx::Error> {
    let job_result = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update ingest_job
         set {}
         where id = $1
           and queue_state = 'leased'",
        super::super::jobs::INGEST_JOB_UPDATE_ASSIGNMENTS,
    )))
    .bind(finalized_attempt.job_id)
    .bind(job_input.mutation_id)
    .bind(job_input.connector_id)
    .bind(job_input.async_operation_id)
    .bind(job_input.knowledge_document_id)
    .bind(job_input.knowledge_revision_id)
    .bind(&job_input.job_kind)
    .bind(&job_input.queue_state)
    .bind(job_input.priority)
    .bind(&job_input.dedupe_key)
    .bind(job_input.available_at)
    .bind(job_input.completed_at)
    .execute(&mut **tx)
    .await?;

    if job_result.rows_affected() == 0 {
        return Ok(false);
    }

    if let Some(event) = lifecycle_event {
        webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(&mut **tx, event)
            .await?;
        catalog_repository::touch_library_source_truth_version_with_executor(
            &mut **tx,
            event.library_id,
        )
        .await?;
    }
    Ok(true)
}

pub async fn abandon_paused_ingest_attempt(
    postgres: &PgPool,
    attempt_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'abandoned',
             heartbeat_at = now(),
             finished_at = now(),
             failure_class = 'operator_control',
             failure_code = 'paused_by_operator',
             failure_message = 'Processing was paused from the administration queue',
             retryable = true
         where id = $1 and attempt_state = 'leased'",
    )
    .bind(attempt_id)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod terminal_runtime_state_tests {
    use super::*;

    #[test]
    fn terminal_attempt_states_map_to_runtime_rows() {
        assert_eq!(terminal_runtime_states("succeeded"), Some(("failed", "failed")));
        assert_eq!(terminal_runtime_states("failed"), Some(("failed", "failed")));
        assert_eq!(terminal_runtime_states("canceled"), Some(("canceled", "canceled")));
    }

    #[test]
    fn non_terminal_attempt_states_leave_runtime_rows_untouched() {
        assert_eq!(terminal_runtime_states("leased"), None);
        assert_eq!(terminal_runtime_states("running"), None);
        assert_eq!(terminal_runtime_states("pending"), None);
        assert_eq!(terminal_runtime_states("abandoned"), None);
    }
}
