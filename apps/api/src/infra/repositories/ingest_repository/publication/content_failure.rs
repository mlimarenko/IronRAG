use super::*;
use crate::domains::ops::OpsFailureDisposition;

/// The locked job must be the exact content-mutation job named by the failure
/// input; any identity drift is a caller defect, not a stale-authority no-op.
fn ensure_failure_job_matches_input(
    job: &PublicationJobAuthorityRow,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.workspace_id == input.workspace_id, "failure workspace mismatch");
    anyhow::ensure!(job.library_id == input.library_id, "failure library mismatch");
    anyhow::ensure!(job.mutation_id == Some(input.mutation_id), "failure mutation mismatch");
    anyhow::ensure!(
        job.mutation_item_id == Some(input.mutation_item_id),
        "failure mutation item mismatch"
    );
    anyhow::ensure!(
        job.knowledge_document_id == Some(input.document_id),
        "failure document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(input.revision_id),
        "failure revision mismatch"
    );
    anyhow::ensure!(job.job_kind == "content_mutation", "failure job kind mismatch");
    Ok(())
}

fn failure_message_for_attempt(
    message: Option<String>,
    budget_exhausted: bool,
    attempt_number: i32,
) -> String {
    let base = message.unwrap_or_else(|| "ingest attempt failed".to_string());
    if budget_exhausted {
        format!("{base} (exhausted retry budget after {attempt_number} attempts)")
    } else {
        base
    }
}

/// Fails the exact mutation item and, when this attempt owns the aggregate,
/// the mutation itself.
async fn fail_mutation_item_and_aggregate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
    failure_message: &str,
    mutation_failed: bool,
) -> anyhow::Result<()> {
    let item_update = sqlx::query(
        "update content_mutation_item
         set item_state = 'failed',
             message = $5
         where id = $1
           and mutation_id = $2
           and document_id = $3
           and result_revision_id = $4",
    )
    .bind(input.mutation_item_id)
    .bind(input.mutation_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .bind(failure_message)
    .execute(&mut **transaction)
    .await
    .context("fail exact content-ingest mutation item")?;
    anyhow::ensure!(item_update.rows_affected() == 1, "failure item identity changed");
    if !mutation_failed {
        return Ok(());
    }
    let mutation = sqlx::query(
        "update content_mutation
         set mutation_state = 'failed',
             completed_at = $2,
             failure_code = $3,
             conflict_code = null
         where id = $1
           and workspace_id = $4
           and library_id = $5",
    )
    .bind(input.mutation_id)
    .bind(input.failed_at)
    .bind(&input.failure_code)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .execute(&mut **transaction)
    .await
    .context("fail aggregate content mutation")?;
    anyhow::ensure!(mutation.rows_affected() == 1, "failure mutation identity changed");
    Ok(())
}

async fn requeue_failed_job(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
    available_at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<()> {
    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'queued',
             available_at = $2,
             completed_at = null,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'",
    )
    .bind(job_id)
    .bind(available_at)
    .execute(&mut **transaction)
    .await
    .context("requeue failed content-ingest job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "failure job lost its lease");
    Ok(())
}

async fn terminally_fail_job(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<()> {
    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'failed',
             completed_at = $2,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'",
    )
    .bind(job_id)
    .bind(input.failed_at)
    .execute(&mut **transaction)
    .await
    .context("terminally fail content-ingest job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "failure job lost its lease");
    super::super::jobs::reconcile_terminal_failed_preparations_in_transaction(
        transaction,
        Some(input.revision_id),
        None,
    )
    .await
    .context("terminalize failed revision preparation")?;
    Ok(())
}

/// Atomically publishes failed revision readiness and the exact queue,
/// mutation-item, aggregate, and async-operation transition owned by the
/// current content-ingest attempt. It deliberately fences against the current
/// source generation rather than a pre-provider snapshot because exact partial
/// vector cleanup may already have advanced that generation.
pub async fn fail_content_ingest_attempt(
    postgres: &PgPool,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<FailContentIngestAttemptOutcome> {
    let mut transaction = postgres.begin().await.context("begin content-ingest failure")?;
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, input.library_id).await?;

    let (job, attempt) =
        match lock_failed_content_authority(&mut transaction, input, observed_source_truth_version)
            .await?
        {
            FailedContentAuthority::Lost { source_truth_version, commit_context } => {
                transaction.commit().await.context(commit_context)?;
                return Ok(FailContentIngestAttemptOutcome::AuthorityLost { source_truth_version });
            }
            FailedContentAuthority::Locked { job, attempt } => (job, attempt),
        };

    let deleted = fail_content_revision_readiness(&mut transaction, input).await?;

    lock_failed_mutation_scope(&mut transaction, input).await?;

    let web_run_owns_aggregate = web_run_owns_content_aggregate(&mut transaction, input).await?;

    let plan = plan_failed_attempt(input, &attempt, web_run_owns_aggregate);
    if !plan.retry_scheduled {
        fail_mutation_item_and_aggregate(
            &mut transaction,
            input,
            &plan.failure_message,
            plan.mutation_failed,
        )
        .await?;
    }

    terminalize_failed_content_attempt(&mut transaction, input, &job, &attempt, &plan).await?;

    if let Some(async_operation_id) = job.async_operation_id
        && !web_run_owns_aggregate
    {
        transition_failed_content_operation(
            &mut transaction,
            input,
            async_operation_id,
            plan.retry_scheduled,
        )
        .await?;
    }

    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        input.library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit content-ingest failure")?;

    Ok(FailContentIngestAttemptOutcome::Applied {
        deleted,
        source_truth_version,
        retry_scheduled: plan.retry_scheduled,
        mutation_failed: plan.mutation_failed,
    })
}

/// Locked authority for one failed content-ingest attempt. A stale or
/// superseded lease yields `Lost`, which the caller turns into the commit-and-
/// no-op outcome; the job and attempt rows are revalidated against the exact
/// input before `Locked` is returned.
enum FailedContentAuthority {
    Lost { source_truth_version: i64, commit_context: &'static str },
    Locked { job: PublicationJobAuthorityRow, attempt: PublicationAttemptAuthorityRow },
}

/// Locks the exact content-mutation job named by the failure input, proves it
/// matches, then locks the newest leased attempt for that job. Lock order stays
/// library -> job -> attempt.
async fn lock_failed_content_authority(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
    observed_source_truth_version: i64,
) -> anyhow::Result<FailedContentAuthority> {
    let job = super::content_success::lock_publication_job(
        transaction,
        input.attempt_id,
        false,
        "lock failed content-ingest job",
    )
    .await?;
    let Some(job) = job else {
        return Ok(FailedContentAuthority::Lost {
            source_truth_version: observed_source_truth_version,
            commit_context: "commit stale content-ingest failure no-op",
        });
    };
    ensure_failure_job_matches_input(&job, input)?;

    let attempt = sqlx::query_as::<_, PublicationAttemptAuthorityRow>(
        "select attempt.id, attempt.attempt_number, attempt.failure_message
         from ingest_attempt as attempt
         where attempt.id = $1
           and attempt.job_id = $2
           and attempt.attempt_state = 'leased'
           and not exists (
               select 1
               from ingest_attempt as newer
               where newer.job_id = attempt.job_id
                 and newer.attempt_number > attempt.attempt_number
           )
         for update of attempt",
    )
    .bind(input.attempt_id)
    .bind(job.id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock latest failed content-ingest attempt")?;
    let Some(attempt) = attempt else {
        return Ok(FailedContentAuthority::Lost {
            source_truth_version: observed_source_truth_version,
            commit_context: "commit superseded content-ingest failure no-op",
        });
    };
    anyhow::ensure!(attempt.id == input.attempt_id, "failure attempt identity changed");
    Ok(FailedContentAuthority::Locked { job, attempt })
}

/// Locks the exact revision, optionally deletes its partial chunk vectors, and
/// publishes the failed readiness flags. Lock order stays revision before
/// mutation. Returns the number of deleted vectors.
async fn fail_content_revision_readiness(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<u64> {
    let revision_exists = sqlx::query_scalar::<_, Uuid>(
        "select revision_id
         from knowledge_revision
         where revision_id = $1
           and library_id = $2
         for update",
    )
    .bind(input.revision_id)
    .bind(input.library_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock failed content-ingest revision")?;
    anyhow::ensure!(revision_exists.is_some(), "failed publication revision disappeared");

    let deleted = if input.delete_vectors {
        delete_ingest_revision_chunk_vectors(transaction, input.library_id, input.revision_id)
            .await?
    } else {
        0
    };
    let readiness = sqlx::query(
        "update knowledge_revision
         set vector_state = 'failed',
             graph_state = 'failed',
             vector_ready_at = null,
             graph_ready_at = null
         where revision_id = $1
           and library_id = $2",
    )
    .bind(input.revision_id)
    .bind(input.library_id)
    .execute(&mut **transaction)
    .await
    .context("publish failed content-ingest readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "failed publication revision disappeared");
    Ok(deleted)
}

/// Locks the aggregate mutation and the exact mutation item and proves both
/// still belong to the input's workspace, library, document, and revision. The
/// item may only be `pending` or `failed` at this point.
async fn lock_failed_mutation_scope(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<()> {
    let mutation = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_mutation
         where id = $1
         for update",
    )
    .bind(input.mutation_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock failed content-ingest mutation")?
    .context("failed publication mutation disappeared")?;
    anyhow::ensure!(mutation.0 == input.workspace_id, "failure mutation workspace mismatch");
    anyhow::ensure!(mutation.1 == input.library_id, "failure mutation library mismatch");

    let item = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, String)>(
        "select mutation_id, document_id, result_revision_id, item_state::text
         from content_mutation_item
         where id = $1
         for update",
    )
    .bind(input.mutation_item_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock failed content-ingest mutation item")?
    .context("failed publication mutation item disappeared")?;
    ensure_failed_item_matches_input(&item, input)
}

/// The locked item must belong to the failure input's mutation, document, and
/// revision, and may only be `pending` or `failed`.
fn ensure_failed_item_matches_input(
    item: &(Uuid, Option<Uuid>, Option<Uuid>, String),
    input: &FailContentIngestAttempt,
) -> anyhow::Result<()> {
    anyhow::ensure!(item.0 == input.mutation_id, "failure item mutation mismatch");
    anyhow::ensure!(item.1 == Some(input.document_id), "failure item document mismatch");
    anyhow::ensure!(item.2 == Some(input.revision_id), "failure item revision mismatch");
    anyhow::ensure!(
        item.3 == "pending" || item.3 == "failed",
        "failure item has incompatible terminal state {}",
        item.3
    );
    Ok(())
}

/// Reports whether the mutation's aggregate is owned by a web ingest run. A
/// web-owned aggregate is finalized by run orchestration, not by one item.
async fn web_run_owns_content_aggregate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
) -> anyhow::Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from content_web_ingest_run
            where mutation_id = $1
         )",
    )
    .bind(input.mutation_id)
    .fetch_one(&mut **transaction)
    .await
    .context("classify failed web-owned mutation aggregate")
}

/// Terminalizes the failed attempt: CAS-fences it on `leased`, closes the
/// graph-extraction runtime rows it activated in the same transaction, and
/// either requeues the still-retryable job or terminally fails it.
async fn terminalize_failed_content_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
    job: &PublicationJobAuthorityRow,
    attempt: &PublicationAttemptAuthorityRow,
    plan: &FailedAttemptPlan,
) -> anyhow::Result<()> {
    let attempt_update = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'failed',
             current_stage = coalesce($2, current_stage),
             heartbeat_at = $3,
             finished_at = $3,
             failure_class = $4,
             failure_code = $5,
             failure_message = $6,
             retryable = $7
         where id = $1
           and job_id = $8
           and attempt_state = 'leased'",
    )
    .bind(input.attempt_id)
    .bind(&input.current_stage)
    .bind(input.failed_at)
    .bind(&input.failure_class)
    .bind(&input.failure_code)
    .bind(&plan.failure_message)
    .bind(plan.retry_scheduled)
    .bind(job.id)
    .execute(&mut **transaction)
    .await
    .context("finalize failed content-ingest attempt")?;
    anyhow::ensure!(attempt_update.rows_affected() == 1, "failure attempt lost its lease");

    // The attempt is terminal; close the graph-extraction runtime rows it
    // activated in the same transaction so they cannot outlive it.
    crate::infra::repositories::runtime_graph_repository::terminalize_runtime_rows_for_attempt(
        transaction,
        input.attempt_id,
        "failed",
        "failed",
        input.failure_code.as_deref().unwrap_or(
            crate::infra::repositories::runtime_graph_repository::RECONCILED_ORPHAN_FAILURE_CODE,
        ),
    )
    .await
    .context("close failed content-ingest runtime rows")?;

    if plan.retry_scheduled {
        let available_at = input.failed_at
            + retry_backoff_with_deferral(attempt.attempt_number, input.operational_defer);
        requeue_failed_job(transaction, job.id, available_at).await?;
    } else {
        terminally_fail_job(transaction, job.id, input).await?;
    }
    Ok(())
}

/// Terminal disposition derived from one failed attempt's retry classification
/// and the aggregate ownership of its mutation.
struct FailedAttemptPlan {
    retry_scheduled: bool,
    mutation_failed: bool,
    failure_message: String,
}

/// Pure derivation of the retry/aggregate/failure-message disposition for one
/// failed content-ingest attempt. Kept free of I/O so the branch structure is
/// testable without a database.
fn plan_failed_attempt(
    input: &FailContentIngestAttempt,
    attempt: &PublicationAttemptAuthorityRow,
    web_run_owns_aggregate: bool,
) -> FailedAttemptPlan {
    let operationally_deferred = input.operational_defer.is_some();
    let budget_exhausted = is_retry_budget_exhausted(
        "failed",
        input.retryable,
        attempt.attempt_number,
        operationally_deferred,
    );
    let retry_scheduled = (input.retryable && !budget_exhausted) || operationally_deferred;
    let failure_message = failure_message_for_attempt(
        input.failure_message.clone().or_else(|| attempt.failure_message.clone()),
        budget_exhausted,
        attempt.attempt_number,
    );
    FailedAttemptPlan {
        retry_scheduled,
        mutation_failed: !retry_scheduled && !web_run_owns_aggregate,
        failure_message,
    }
}

/// Transitions the linked content-mutation async operation for a failed attempt:
/// a requeue keeps it running without a disposition, a terminal failure records
/// the disposition. The exact operation identity row must exist.
async fn transition_failed_content_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &FailContentIngestAttempt,
    async_operation_id: Uuid,
    retry_scheduled: bool,
) -> anyhow::Result<()> {
    let (status, completed_at, failure_code, failure_disposition) = if retry_scheduled {
        // A requeue is not a terminal proof: the operation keeps running
        // and must not carry a disposition from this attempt.
        ("accepted", None, None, None)
    } else {
        (
            "failed",
            Some(input.failed_at),
            input.failure_code.as_deref(),
            input.failure_disposition.map(OpsFailureDisposition::as_str),
        )
    };
    let operation = sqlx::query(
        "update ops_async_operation
             set status = $2::ops_async_operation_status,
                 completed_at = $3,
                 failure_code = $4,
                 failure_disposition = $5
             where id = $1
               and workspace_id = $6
               and library_id = $7
               and subject_kind = 'content_mutation'
               and subject_id = $8",
    )
    .bind(async_operation_id)
    .bind(status)
    .bind(completed_at)
    .bind(failure_code)
    .bind(failure_disposition)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .execute(&mut **transaction)
    .await
    .context("transition failed content-mutation async operation")?;
    anyhow::ensure!(
        operation.rows_affected() == 1,
        "failed content-mutation async operation identity changed"
    );
    Ok(())
}

#[cfg(test)]
mod failed_attempt_plan_tests {
    use super::*;
    use crate::domains::ingest::MAX_INGEST_ATTEMPTS;

    fn input(retryable: bool, defer: Option<chrono::Duration>) -> FailContentIngestAttempt {
        FailContentIngestAttempt {
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            document_id: Uuid::from_u128(3),
            revision_id: Uuid::from_u128(4),
            mutation_id: Uuid::from_u128(5),
            mutation_item_id: Uuid::from_u128(6),
            attempt_id: Uuid::from_u128(7),
            current_stage: None,
            failure_class: None,
            failure_code: None,
            failure_disposition: None,
            failure_message: None,
            retryable,
            operational_defer: defer,
            delete_vectors: false,
            failed_at: chrono::Utc::now(),
        }
    }

    fn attempt(number: i32, message: Option<&str>) -> PublicationAttemptAuthorityRow {
        PublicationAttemptAuthorityRow {
            id: Uuid::from_u128(7),
            attempt_number: number,
            failure_message: message.map(str::to_string),
        }
    }

    #[test]
    fn retryable_attempt_below_budget_is_requeued_without_failing_the_mutation() {
        let plan = plan_failed_attempt(&input(true, None), &attempt(1, None), false);
        assert!(plan.retry_scheduled);
        assert!(!plan.mutation_failed);
        assert_eq!(plan.failure_message, "ingest attempt failed");
    }

    #[test]
    fn retryable_attempt_at_budget_exhaustion_fails_the_mutation() {
        let plan =
            plan_failed_attempt(&input(true, None), &attempt(MAX_INGEST_ATTEMPTS, None), false);
        assert!(!plan.retry_scheduled);
        assert!(plan.mutation_failed);
        assert_eq!(
            plan.failure_message,
            format!(
                "ingest attempt failed (exhausted retry budget after {MAX_INGEST_ATTEMPTS} attempts)"
            )
        );
    }

    #[test]
    fn operational_defer_requeues_even_after_budget_exhaustion() {
        let plan = plan_failed_attempt(
            &input(true, Some(chrono::Duration::seconds(30))),
            &attempt(MAX_INGEST_ATTEMPTS, None),
            false,
        );
        assert!(plan.retry_scheduled);
        assert!(!plan.mutation_failed);
    }

    #[test]
    fn web_owned_aggregate_is_never_failed_by_one_item() {
        let plan = plan_failed_attempt(&input(false, None), &attempt(1, None), true);
        assert!(!plan.retry_scheduled);
        assert!(!plan.mutation_failed);
    }

    #[test]
    fn attempt_failure_message_is_used_when_input_omits_one() {
        let plan =
            plan_failed_attempt(&input(false, None), &attempt(1, Some("provider exploded")), false);
        assert_eq!(plan.failure_message, "provider exploded");
    }

    fn item(state: &str) -> (Uuid, Option<Uuid>, Option<Uuid>, String) {
        (Uuid::from_u128(5), Some(Uuid::from_u128(3)), Some(Uuid::from_u128(4)), state.to_string())
    }

    #[test]
    fn failed_item_accepts_pending_and_failed_terminal_states() {
        assert!(ensure_failed_item_matches_input(&item("pending"), &input(false, None)).is_ok());
        assert!(ensure_failed_item_matches_input(&item("failed"), &input(false, None)).is_ok());
    }

    #[test]
    fn failed_item_rejects_an_applied_or_foreign_state() {
        assert!(ensure_failed_item_matches_input(&item("applied"), &input(false, None)).is_err());
        assert!(ensure_failed_item_matches_input(&item("skipped"), &input(false, None)).is_err());
    }

    #[test]
    fn failed_item_rejects_identity_drift() {
        let mut foreign = item("pending");
        foreign.0 = Uuid::from_u128(99);
        assert!(ensure_failed_item_matches_input(&foreign, &input(false, None)).is_err());
        let mut other_revision = item("pending");
        other_revision.2 = Some(Uuid::from_u128(99));
        assert!(ensure_failed_item_matches_input(&other_revision, &input(false, None)).is_err());
    }
}
