use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    domains::{
        ingest::{is_retry_budget_exhausted, retry_backoff_after_attempt},
        webhook::{WebhookEvent, revision_ready_event_id},
    },
    infra::{
        postgres::pg_search_store::{
            advance_vector_source_truth_version, delete_ingest_revision_chunk_vectors,
            lock_exclusive_vector_mutation_source, validate_ingest_revision_vector_coverage,
        },
        repositories::{
            catalog_repository,
            content_repository::{
                self, MaterializeKnowledgeDocumentOutcome, NewContentDocumentHead,
            },
            webhook_outbox_repository,
        },
    },
};

#[derive(Debug, FromRow)]
struct PublicationJobAuthorityRow {
    id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    mutation_id: Option<Uuid>,
    mutation_item_id: Option<Uuid>,
    async_operation_id: Option<Uuid>,
    knowledge_document_id: Option<Uuid>,
    knowledge_revision_id: Option<Uuid>,
    job_kind: String,
}

#[derive(Debug, FromRow)]
struct PublicationAttemptAuthorityRow {
    id: Uuid,
    attempt_number: i32,
    failure_message: Option<String>,
}

/// Exact identities and source/profile snapshot required to atomically publish
/// one successful content-ingest attempt.
///
/// The repository revalidates every identity while holding the library, job,
/// and attempt locks. Callers must treat
/// [`PublishContentIngestSuccessOutcome::AuthorityLost`] as a successful stale-worker no-op and
/// must not run a second lifecycle finalizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishContentIngestSuccess {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_source_truth_version: i64,
    pub embedding_profile_key: Option<String>,
    pub text_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishContentIngestSuccessOutcome {
    Applied { source_truth_version: i64, mutation_completed: bool },
    AuthorityLost { source_truth_version: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailContentIngestAttempt {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub current_stage: Option<String>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub retryable: bool,
    pub delete_vectors: bool,
    pub failed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailContentIngestAttemptOutcome {
    Applied {
        deleted: u64,
        source_truth_version: i64,
        retry_scheduled: bool,
        mutation_failed: bool,
    },
    AuthorityLost {
        source_truth_version: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishGraphRefreshSuccess {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Uuid,
    pub graph_state: String,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishGraphRefreshSuccessOutcome {
    Applied { source_truth_version: i64 },
    Superseded { source_truth_version: i64 },
    AuthorityLost { source_truth_version: i64 },
}

async fn lock_library_publication_source(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut **transaction,
            workspace_id,
            library_id,
        )
        .await
        .context("lock graph-refresh publication library")?,
        "graph-refresh publication library disappeared"
    );
    sqlx::query_scalar::<_, i64>(
        "select source_truth_version
         from catalog_library
         where id = $1 and workspace_id = $2",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .context("read graph-refresh publication source generation")
}

/// Publishes revision readiness, canonical head/projection, exact mutation
/// item, aggregate-safe mutation state, attempt/job completion, and durable
/// webhook handoff in one transaction with one source-generation advance.
pub async fn publish_content_ingest_success(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    anyhow::ensure!(
        input.expected_source_truth_version > 0,
        "content-ingest publication requires a positive source-truth fence"
    );

    let mut transaction = postgres.begin().await.context("begin content-ingest publication")?;
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, input.library_id).await?;

    // Canonical lock order is library -> job -> attempt. Lock the job through
    // the supplied attempt identity first, then lock and prove that exact
    // attempt is still the newest lease for the job.
    let job = sqlx::query_as::<_, PublicationJobAuthorityRow>(
        "select
            job.id,
            job.workspace_id,
            job.library_id,
            job.mutation_id,
            job.mutation_item_id,
            job.async_operation_id,
            job.knowledge_document_id,
            job.knowledge_revision_id,
            job.job_kind::text as job_kind
         from ingest_attempt as attempt
         join ingest_job as job on job.id = attempt.job_id
         where attempt.id = $1
           and job.queue_state = 'leased'
         for update of job",
    )
    .bind(input.attempt_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock content-ingest publication job")?;
    let Some(job) = job else {
        transaction.commit().await.context("commit stale content-ingest publication no-op")?;
        return Ok(PublishContentIngestSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    };

    anyhow::ensure!(job.workspace_id == input.workspace_id, "publication workspace mismatch");
    anyhow::ensure!(job.library_id == input.library_id, "publication library mismatch");
    anyhow::ensure!(job.mutation_id == Some(input.mutation_id), "publication mutation mismatch");
    anyhow::ensure!(
        job.mutation_item_id == Some(input.mutation_item_id),
        "publication mutation item mismatch"
    );
    anyhow::ensure!(
        job.knowledge_document_id == Some(input.document_id),
        "publication document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(input.revision_id),
        "publication revision mismatch"
    );
    anyhow::ensure!(job.job_kind == "content_mutation", "publication job kind mismatch");

    let authoritative_attempt_id = sqlx::query_scalar::<_, Uuid>(
        "select attempt.id
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
    .fetch_optional(&mut *transaction)
    .await
    .context("lock latest content-ingest publication attempt")?;
    if authoritative_attempt_id != Some(input.attempt_id) {
        transaction.commit().await.context("commit superseded content-ingest publication no-op")?;
        return Ok(PublishContentIngestSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    }

    anyhow::ensure!(
        observed_source_truth_version == input.expected_source_truth_version,
        "library source or embedding profile changed before content-ingest publication"
    );

    validate_ingest_revision_vector_coverage(
        &mut transaction,
        input.library_id,
        input.revision_id,
        input.embedding_profile_key.as_deref(),
    )
    .await?;

    let readiness = sqlx::query(
        "update knowledge_revision
         set text_state = $3,
             vector_state = 'ready',
             graph_state = $4,
             text_readable_at = $5,
             vector_ready_at = coalesce(vector_ready_at, $7),
             graph_ready_at = $6
         where revision_id = $1
           and library_id = $2",
    )
    .bind(input.revision_id)
    .bind(input.library_id)
    .bind(&input.text_state)
    .bind(&input.graph_state)
    .bind(input.text_readable_at)
    .bind(input.graph_ready_at)
    .bind(input.completed_at)
    .execute(&mut *transaction)
    .await
    .context("publish content-ingest revision readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "publication revision disappeared");

    let mutation = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_mutation
         where id = $1
         for update",
    )
    .bind(input.mutation_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock content-ingest publication mutation")?
    .context("publication mutation disappeared")?;
    anyhow::ensure!(mutation.0 == input.workspace_id, "publication mutation workspace mismatch");
    anyhow::ensure!(mutation.1 == input.library_id, "publication mutation library mismatch");

    let item = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, String)>(
        "select mutation_id, document_id, result_revision_id, item_state::text
         from content_mutation_item
         where id = $1
         for update",
    )
    .bind(input.mutation_item_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock exact content-ingest mutation item")?
    .context("publication mutation item disappeared")?;
    anyhow::ensure!(item.0 == input.mutation_id, "publication item mutation mismatch");
    anyhow::ensure!(item.1 == Some(input.document_id), "publication item document mismatch");
    anyhow::ensure!(item.2 == Some(input.revision_id), "publication item result revision mismatch");
    anyhow::ensure!(
        item.3 == "pending" || item.3 == "applied",
        "publication item is already terminal with incompatible state {}",
        item.3
    );

    let item_update = sqlx::query(
        "update content_mutation_item
         set item_state = 'applied',
             message = 'mutation applied by canonical ingest publication'
         where id = $1
           and mutation_id = $2
           and document_id = $3
           and result_revision_id = $4",
    )
    .bind(input.mutation_item_id)
    .bind(input.mutation_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut *transaction)
    .await
    .context("apply exact content-ingest mutation item")?;
    anyhow::ensure!(item_update.rows_affected() == 1, "publication item identity changed");

    // Web run orchestration owns its aggregate mutation because child page
    // ingests can finish independently. Ordinary mutations complete only once
    // every item is an acceptable success; no first-item shortcut is valid.
    let web_run_owns_aggregate = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from content_web_ingest_run
            where mutation_id = $1
         )",
    )
    .bind(input.mutation_id)
    .fetch_one(&mut *transaction)
    .await
    .context("classify web-owned mutation aggregate")?;
    let mutation_completed = if web_run_owns_aggregate {
        false
    } else {
        let all_items_acceptable = sqlx::query_scalar::<_, bool>(
            "select not exists (
                select 1
                from content_mutation_item
                where mutation_id = $1
                  and item_state not in ('applied', 'skipped')
             )",
        )
        .bind(input.mutation_id)
        .fetch_one(&mut *transaction)
        .await
        .context("check aggregate mutation completion")?;
        if all_items_acceptable {
            let aggregate = sqlx::query(
                "update content_mutation
                 set mutation_state = 'applied',
                     completed_at = $2,
                     failure_code = null,
                     conflict_code = null
                 where id = $1",
            )
            .bind(input.mutation_id)
            .bind(input.completed_at)
            .execute(&mut *transaction)
            .await
            .context("complete aggregate content mutation")?;
            anyhow::ensure!(aggregate.rows_affected() == 1, "publication mutation disappeared");
        }
        all_items_acceptable
    };

    let previous_head = content_repository::get_document_head_for_update_with_executor(
        &mut *transaction,
        input.document_id,
    )
    .await
    .context("lock content document head for ingest publication")?
    .context("content document head disappeared before ingest publication")?;
    anyhow::ensure!(
        previous_head.latest_mutation_id == Some(input.mutation_id),
        "content document advanced to another mutation before ingest publication"
    );
    content_repository::upsert_document_head_without_generation_with_executor(
        &mut *transaction,
        &NewContentDocumentHead {
            document_id: input.document_id,
            active_revision_id: Some(input.revision_id),
            readable_revision_id: Some(input.revision_id),
            latest_mutation_id: Some(input.mutation_id),
            latest_successful_attempt_id: Some(input.attempt_id),
        },
    )
    .await
    .context("publish canonical content document head")?;
    let materialized =
        content_repository::materialize_knowledge_document_from_canonical_head_with_transaction(
            &mut transaction,
            input.document_id,
        )
        .await
        .context("materialize content-ingest knowledge projection")?;
    anyhow::ensure!(
        materialized.outcome == MaterializeKnowledgeDocumentOutcome::Materialized,
        "content-ingest projection could not materialize: {:?}",
        materialized.outcome
    );

    let attempt = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'succeeded',
             current_stage = 'finalizing',
             heartbeat_at = $2,
             finished_at = $2,
             failure_class = null,
             failure_code = null,
             failure_message = null,
             progress_percent = 100,
             retryable = false
         where id = $1
           and job_id = $3
           and attempt_state = 'leased'",
    )
    .bind(input.attempt_id)
    .bind(input.completed_at)
    .bind(job.id)
    .execute(&mut *transaction)
    .await
    .context("complete content-ingest attempt")?;
    anyhow::ensure!(attempt.rows_affected() == 1, "publication attempt lost its lease");

    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = $2,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'
           and workspace_id = $3
           and library_id = $4
           and mutation_id = $5
           and mutation_item_id = $6
           and knowledge_document_id = $7
           and knowledge_revision_id = $8",
    )
    .bind(job.id)
    .bind(input.completed_at)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.mutation_item_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut *transaction)
    .await
    .context("complete content-ingest job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "publication job lost its lease");

    if mutation_completed && let Some(async_operation_id) = job.async_operation_id {
        let operation = sqlx::query(
            "update ops_async_operation
                 set status = 'ready',
                     completed_at = $2,
                     failure_code = null
                 where id = $1
                   and workspace_id = $3
                   and library_id = $4
                   and subject_kind = 'content_mutation'
                   and subject_id = $5",
        )
        .bind(async_operation_id)
        .bind(input.completed_at)
        .bind(input.workspace_id)
        .bind(input.library_id)
        .bind(input.mutation_id)
        .execute(&mut *transaction)
        .await
        .context("complete linked content-mutation async operation")?;
        anyhow::ensure!(
            operation.rows_affected() == 1,
            "linked content-mutation async operation identity changed"
        );
    }

    let event = WebhookEvent {
        event_type: "revision.ready".to_string(),
        event_id: revision_ready_event_id(input.revision_id),
        occurred_at: input.completed_at,
        workspace_id: input.workspace_id,
        library_id: input.library_id,
        payload_json: serde_json::json!({
            "document_id": input.document_id,
            "revision_id": input.revision_id,
            "library_id": input.library_id,
        }),
    };
    webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(
        &mut *transaction,
        &event,
    )
    .await
    .context("enqueue deterministic revision-ready lifecycle event")?;

    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        input.library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit content-ingest publication")?;

    Ok(PublishContentIngestSuccessOutcome::Applied { source_truth_version, mutation_completed })
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

    let job = sqlx::query_as::<_, PublicationJobAuthorityRow>(
        "select
            job.id,
            job.workspace_id,
            job.library_id,
            job.mutation_id,
            job.mutation_item_id,
            job.async_operation_id,
            job.knowledge_document_id,
            job.knowledge_revision_id,
            job.job_kind::text as job_kind
         from ingest_attempt as attempt
         join ingest_job as job on job.id = attempt.job_id
         where attempt.id = $1
           and job.queue_state = 'leased'
         for update of job",
    )
    .bind(input.attempt_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock failed content-ingest job")?;
    let Some(job) = job else {
        transaction.commit().await.context("commit stale content-ingest failure no-op")?;
        return Ok(FailContentIngestAttemptOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    };
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
    .fetch_optional(&mut *transaction)
    .await
    .context("lock latest failed content-ingest attempt")?;
    let Some(attempt) = attempt else {
        transaction.commit().await.context("commit superseded content-ingest failure no-op")?;
        return Ok(FailContentIngestAttemptOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    };
    anyhow::ensure!(attempt.id == input.attempt_id, "failure attempt identity changed");

    let revision_exists = sqlx::query_scalar::<_, Uuid>(
        "select revision_id
         from knowledge_revision
         where revision_id = $1
           and library_id = $2
         for update",
    )
    .bind(input.revision_id)
    .bind(input.library_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock failed content-ingest revision")?;
    anyhow::ensure!(revision_exists.is_some(), "failed publication revision disappeared");

    let deleted = if input.delete_vectors {
        delete_ingest_revision_chunk_vectors(&mut transaction, input.library_id, input.revision_id)
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
    .execute(&mut *transaction)
    .await
    .context("publish failed content-ingest readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "failed publication revision disappeared");

    let mutation = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_mutation
         where id = $1
         for update",
    )
    .bind(input.mutation_id)
    .fetch_optional(&mut *transaction)
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
    .fetch_optional(&mut *transaction)
    .await
    .context("lock failed content-ingest mutation item")?
    .context("failed publication mutation item disappeared")?;
    anyhow::ensure!(item.0 == input.mutation_id, "failure item mutation mismatch");
    anyhow::ensure!(item.1 == Some(input.document_id), "failure item document mismatch");
    anyhow::ensure!(item.2 == Some(input.revision_id), "failure item revision mismatch");
    anyhow::ensure!(
        item.3 == "pending" || item.3 == "failed",
        "failure item has incompatible terminal state {}",
        item.3
    );

    let budget_exhausted =
        is_retry_budget_exhausted("failed", input.retryable, attempt.attempt_number);
    let retry_scheduled = input.retryable && !budget_exhausted;
    let base_failure_message = input
        .failure_message
        .clone()
        .or(attempt.failure_message)
        .unwrap_or_else(|| "ingest attempt failed".to_string());
    let failure_message = if budget_exhausted {
        format!(
            "{base_failure_message} (exhausted retry budget after {} attempts)",
            attempt.attempt_number
        )
    } else {
        base_failure_message
    };

    let web_run_owns_aggregate = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from content_web_ingest_run
            where mutation_id = $1
         )",
    )
    .bind(input.mutation_id)
    .fetch_one(&mut *transaction)
    .await
    .context("classify failed web-owned mutation aggregate")?;
    let mutation_failed = !retry_scheduled && !web_run_owns_aggregate;
    if !retry_scheduled {
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
        .bind(&failure_message)
        .execute(&mut *transaction)
        .await
        .context("fail exact content-ingest mutation item")?;
        anyhow::ensure!(item_update.rows_affected() == 1, "failure item identity changed");

        if mutation_failed {
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
            .execute(&mut *transaction)
            .await
            .context("fail aggregate content mutation")?;
            anyhow::ensure!(mutation.rows_affected() == 1, "failure mutation identity changed");
        }
    }

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
    .bind(&failure_message)
    .bind(retry_scheduled)
    .bind(job.id)
    .execute(&mut *transaction)
    .await
    .context("finalize failed content-ingest attempt")?;
    anyhow::ensure!(attempt_update.rows_affected() == 1, "failure attempt lost its lease");

    if retry_scheduled {
        let available_at = input.failed_at + retry_backoff_after_attempt(attempt.attempt_number);
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
        .bind(job.id)
        .bind(available_at)
        .execute(&mut *transaction)
        .await
        .context("requeue failed content-ingest job")?;
        anyhow::ensure!(job_update.rows_affected() == 1, "failure job lost its lease");
    } else {
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
        .bind(job.id)
        .bind(input.failed_at)
        .execute(&mut *transaction)
        .await
        .context("terminally fail content-ingest job")?;
        anyhow::ensure!(job_update.rows_affected() == 1, "failure job lost its lease");
    }

    if let Some(async_operation_id) = job.async_operation_id
        && !web_run_owns_aggregate
    {
        let (status, completed_at, failure_code) = if retry_scheduled {
            ("accepted", None, None)
        } else {
            ("failed", Some(input.failed_at), input.failure_code.as_deref())
        };
        let operation = sqlx::query(
            "update ops_async_operation
                 set status = $2::ops_async_operation_status,
                     completed_at = $3,
                     failure_code = $4
                 where id = $1
                   and workspace_id = $5
                   and library_id = $6
                   and subject_kind = 'content_mutation'
                   and subject_id = $7",
        )
        .bind(async_operation_id)
        .bind(status)
        .bind(completed_at)
        .bind(failure_code)
        .bind(input.workspace_id)
        .bind(input.library_id)
        .bind(input.mutation_id)
        .execute(&mut *transaction)
        .await
        .context("transition failed content-mutation async operation")?;
        anyhow::ensure!(
            operation.rows_affected() == 1,
            "failed content-mutation async operation identity changed"
        );
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
        retry_scheduled,
        mutation_failed,
    })
}

/// Publishes graph-only maintenance for the still-current readable revision.
///
/// No mutation is fabricated and no revision-ready event is emitted: the
/// document was already readable before this maintenance job started.
pub async fn publish_graph_refresh_success(
    postgres: &PgPool,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    let mut transaction = postgres.begin().await.context("begin graph-refresh publication")?;
    let observed_source_truth_version =
        lock_library_publication_source(&mut transaction, input.workspace_id, input.library_id)
            .await?;

    let job = sqlx::query_as::<_, PublicationJobAuthorityRow>(
        "select
            job.id,
            job.workspace_id,
            job.library_id,
            job.mutation_id,
            job.mutation_item_id,
            job.async_operation_id,
            job.knowledge_document_id,
            job.knowledge_revision_id,
            job.job_kind::text as job_kind
         from ingest_attempt as attempt
         join ingest_job as job on job.id = attempt.job_id
         where attempt.id = $1
           and job.queue_state = 'leased'
         for update of job",
    )
    .bind(input.attempt_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock graph-refresh publication job")?;
    let Some(job) = job else {
        transaction.commit().await.context("commit stale graph-refresh publication no-op")?;
        return Ok(PublishGraphRefreshSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    };
    anyhow::ensure!(job.workspace_id == input.workspace_id, "graph-refresh workspace mismatch");
    anyhow::ensure!(job.library_id == input.library_id, "graph-refresh library mismatch");
    anyhow::ensure!(job.mutation_id.is_none(), "graph-refresh job unexpectedly owns a mutation");
    anyhow::ensure!(
        job.mutation_item_id.is_none(),
        "graph-refresh job unexpectedly owns a mutation item"
    );
    anyhow::ensure!(
        job.async_operation_id.is_none(),
        "graph-refresh maintenance job unexpectedly owns an async operation"
    );
    anyhow::ensure!(
        job.knowledge_document_id == Some(input.document_id),
        "graph-refresh document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(input.revision_id),
        "graph-refresh revision mismatch"
    );
    anyhow::ensure!(job.job_kind == "graph_refresh", "graph-refresh job kind mismatch");

    let authoritative_attempt_id = sqlx::query_scalar::<_, Uuid>(
        "select attempt.id
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
    .fetch_optional(&mut *transaction)
    .await
    .context("lock latest graph-refresh attempt")?;
    if authoritative_attempt_id != Some(input.attempt_id) {
        transaction.commit().await.context("commit superseded graph-refresh lease no-op")?;
        return Ok(PublishGraphRefreshSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    }

    let head = content_repository::get_document_head_for_update_with_executor(
        &mut *transaction,
        input.document_id,
    )
    .await
    .context("lock graph-refresh document head")?
    .context("graph-refresh document head disappeared")?;
    if head.active_revision_id != Some(input.revision_id)
        || head.readable_revision_id != Some(input.revision_id)
    {
        let attempt = sqlx::query(
            "update ingest_attempt
             set attempt_state = 'abandoned',
                 current_stage = 'finalizing',
                 heartbeat_at = $2,
                 finished_at = $2,
                 failure_class = 'lifecycle',
                 failure_code = 'revision_superseded',
                 failure_message = 'graph refresh revision is no longer the active readable head',
                 retryable = false
             where id = $1
               and job_id = $3
               and attempt_state = 'leased'",
        )
        .bind(input.attempt_id)
        .bind(input.completed_at)
        .bind(job.id)
        .execute(&mut *transaction)
        .await
        .context("abandon superseded graph-refresh attempt")?;
        anyhow::ensure!(attempt.rows_affected() == 1, "graph-refresh attempt lost its lease");
        let job_update = sqlx::query(
            "update ingest_job
             set queue_state = 'completed',
                 completed_at = $2,
                 queue_leased_at = null,
                 queue_lease_token = null,
                 queue_lease_owner = null
             where id = $1
               and queue_state = 'leased'",
        )
        .bind(job.id)
        .bind(input.completed_at)
        .execute(&mut *transaction)
        .await
        .context("complete superseded graph-refresh job")?;
        anyhow::ensure!(job_update.rows_affected() == 1, "graph-refresh job lost its lease");
        transaction.commit().await.context("commit superseded graph-refresh publication")?;
        return Ok(PublishGraphRefreshSuccessOutcome::Superseded {
            source_truth_version: observed_source_truth_version,
        });
    }

    let readiness = sqlx::query(
        "update knowledge_revision
         set graph_state = $4,
             graph_ready_at = $5
         where revision_id = $1
           and document_id = $2
           and library_id = $3",
    )
    .bind(input.revision_id)
    .bind(input.document_id)
    .bind(input.library_id)
    .bind(&input.graph_state)
    .bind(input.graph_ready_at)
    .execute(&mut *transaction)
    .await
    .context("publish graph-refresh readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "graph-refresh revision disappeared");

    let attempt = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'succeeded',
             current_stage = 'finalizing',
             heartbeat_at = $2,
             finished_at = $2,
             failure_class = null,
             failure_code = null,
             failure_message = null,
             progress_percent = 100,
             retryable = false
         where id = $1
           and job_id = $3
           and attempt_state = 'leased'",
    )
    .bind(input.attempt_id)
    .bind(input.completed_at)
    .bind(job.id)
    .execute(&mut *transaction)
    .await
    .context("complete graph-refresh attempt")?;
    anyhow::ensure!(attempt.rows_affected() == 1, "graph-refresh attempt lost its lease");
    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = $2,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'
           and mutation_id is null
           and mutation_item_id is null
           and knowledge_document_id = $3
           and knowledge_revision_id = $4",
    )
    .bind(job.id)
    .bind(input.completed_at)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut *transaction)
    .await
    .context("complete graph-refresh job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "graph-refresh job lost its lease");

    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        input.library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit graph-refresh publication")?;
    Ok(PublishGraphRefreshSuccessOutcome::Applied { source_truth_version })
}
