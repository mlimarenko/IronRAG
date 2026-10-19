//! Integration coverage for the attempt-abort guard and the orphaned
//! runtime-row reconciler.
//!
//! * `worker_abort_guard_*` proves the worker attempt guard's detached
//!   finalizer marks a leased attempt `worker_aborted` and closes the runtime
//!   rows it activated; the disarmed variant proves the normal path cannot
//!   double-finalize.
//! * `cancel_ingest_job_*` proves the attempt-terminal chokepoint closes the
//!   runtime rows inside its own transaction.
//! * `reconcile_*` proves the bounded idempotent sweeper closes the historical
//!   backlog.
//!
//! Run with:
//! `cargo test -p ironrag-backend --features test-support --test it -- --ignored runtime_orphan_reconcile::`

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::{
    infra::repositories::{
        content_document_head_repository, content_document_repository,
        content_knowledge_projection_repository, content_mutation_repository,
        content_revision_repository, ingest_repository, query_repository,
    },
    services::{
        ingest::worker::worker_attempt_abort_guard_for_test,
        maintenance::orphan_runtime_reconcile::reconcile_orphan_runtime_rows_once,
    },
};

use crate::{content_lifecycle_support::ContentLifecycleFixture, graph_reconcile_support};

const WORKER_ABORTED: &str = "worker_aborted";
const RECONCILED: &str = "reconciled_orphaned_attempt";

struct SeededRuntimeRows {
    extraction_id: Uuid,
    execution_id: Uuid,
}

struct RuntimeRowState {
    extraction_status: String,
    extraction_error: Option<String>,
    execution_state: String,
    execution_failure_code: Option<String>,
    active_stage: Option<String>,
    completed: bool,
}

async fn seed_content_document(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<Uuid> {
    let document = content_knowledge_projection_repository::create_document_with_projection(
        pool,
        &content_document_repository::NewContentDocument {
            workspace_id,
            library_id,
            external_key: &format!("runtime-orphan-doc-{}", Uuid::now_v7().simple()),
            document_state: "active",
            created_by_principal_id: None,
            parent_external_key: None,
            parent_document_id: None,
            document_role: "primary",
        },
        Some("graph-source.txt"),
    )
    .await
    .context("failed to create runtime-orphan source and knowledge document")?;
    Ok(document.id)
}

/// Creates private work against a real source revision, not a fabricated job pointer.
async fn seed_content_mutation_job(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    label: &str,
) -> Result<ingest_repository::IngestJobRow> {
    let revision = match content_revision_repository::create_revision_with_projection(
        pool,
        &content_revision_repository::NewContentRevisionProjection {
            admission_identity: None,
            document_id,
            workspace_id,
            library_id,
            content_source_kind: "upload",
            checksum: &format!("sha256:runtime-orphan:{document_id}"),
            mime_type: "text/plain",
            byte_size: 0,
            title: Some("Runtime orphan source"),
            language_code: None,
            source_uri: None,
            document_hint: None,
            storage_key: None,
            created_by_principal_id: None,
        },
    )
    .await?
    {
        content_revision_repository::CreateContentRevisionOutcome::Created(revision) => revision,
        outcome => bail!("runtime-orphan source revision was not created: {outcome:?}"),
    };
    let job = graph_reconcile_support::create_leased_ingest_job(
        pool,
        graph_reconcile_support::LeasedIngestJobSeed {
            workspace_id,
            library_id,
            document_id,
            revision_id: revision.id,
            job_kind: "content_mutation",
            dedupe_key_prefix: label,
        },
    )
    .await?;
    content_document_head_repository::upsert_document_head(
        pool,
        &content_document_head_repository::NewContentDocumentHead {
            document_id,
            active_revision_id: Some(revision.id),
            readable_revision_id: None,
            publishing_mutation_item_id: None,
            latest_mutation_id: job.mutation_id,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    let projected =
        content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
            pool,
            document_id,
        )
        .await?;
    anyhow::ensure!(
        projected == content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized,
        "runtime-orphan private document projection was not materialized"
    );
    Ok(job)
}

/// Inserts one `runtime_execution` owned by `extraction_id` plus the
/// `runtime_graph_extraction` row itself, both stamped with the caller's clock
/// so a test can place them inside or outside the reconciler's stale window.
async fn seed_processing_extraction_for_attempt(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    attempt_id: Uuid,
    created_at: chrono::DateTime<Utc>,
    accepted_at: chrono::DateTime<Utc>,
) -> Result<SeededRuntimeRows> {
    let extraction_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    sqlx::query(
        "insert into runtime_execution (
             id, owner_kind, owner_id, task_kind, surface_kind, contract_name,
             contract_version, lifecycle_state, active_stage, turn_budget, turn_count,
             parallel_action_limit, failure_code, failure_summary_redacted, accepted_at,
             completed_at, parent_execution_id
         ) values (
             $1, 'graph_extraction_attempt', $2, 'graph_extract', 'worker',
             'graph_extract', 'runtime-orphan-reconcile-test', 'running', null,
             1, 0, 1, null, null, $3, null, null
         )",
    )
    .bind(execution_id)
    .bind(extraction_id)
    .bind(accepted_at)
    .execute(pool)
    .await
    .context("failed to insert runtime execution for orphan fixture")?;
    sqlx::query(
        "insert into runtime_graph_extraction (
             id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
             model_name, extraction_version, prompt_hash, status, raw_output_json,
             normalized_output_json, glean_pass_count, error_message, created_at
         ) values (
             $1, $2, $3, $4, $5, 'openai', 'gpt-test', 'v1', 'sha256:prompt',
             'processing',
             jsonb_build_object(
                 'lifecycle',
                 jsonb_build_object('activated_by_attempt_id', $6::text)
             ),
             jsonb_build_object('entities', '[]'::jsonb, 'relations', '[]'::jsonb),
             0, null, $7
         )",
    )
    .bind(extraction_id)
    .bind(execution_id)
    .bind(library_id)
    .bind(document_id)
    // `runtime_graph_extraction.chunk_id` has no foreign key; the reconciler
    // never joins it.
    .bind(Uuid::now_v7())
    .bind(attempt_id)
    .bind(created_at)
    .execute(pool)
    .await
    .context("failed to insert processing runtime graph extraction")?;
    Ok(SeededRuntimeRows { extraction_id, execution_id })
}

/// Overwrites the activation attempt's state directly so a test can model a
/// terminal attempt without running a pipeline.
async fn set_attempt_state(pool: &PgPool, attempt_id: Uuid, state: &str) -> Result<()> {
    sqlx::query(
        "update ingest_attempt
         set attempt_state = $2::ingest_attempt_state,
             finished_at = now()
         where id = $1",
    )
    .bind(attempt_id)
    .bind(state)
    .execute(pool)
    .await
    .context("failed to overwrite orphan-fixture attempt state")?;
    Ok(())
}

/// Seeds a document and a processing extraction activated by an attempt in
/// `attempt_state`, stamped at `seeded_at`. `None` models an activation
/// attempt that no longer exists.
async fn seed_reconcile_case(
    fixture: &ContentLifecycleFixture,
    attempt_state: Option<&str>,
    seeded_at: chrono::DateTime<Utc>,
) -> Result<SeededRuntimeRows> {
    let pool = &fixture.state.persistence.postgres;
    let document_id = seed_content_document(pool, fixture.workspace_id, fixture.library_id).await?;
    let attempt_id = match attempt_state {
        Some(state) => {
            let label = format!("reconcile-{state}");
            let job = seed_content_mutation_job(
                pool,
                fixture.workspace_id,
                fixture.library_id,
                document_id,
                &label,
            )
            .await?;
            let attempt =
                graph_reconcile_support::create_leased_graph_attempt(pool, job.id, &label).await?;
            set_attempt_state(pool, attempt.id, state).await?;
            attempt.id
        }
        None => Uuid::now_v7(),
    };
    seed_processing_extraction_for_attempt(
        pool,
        fixture.library_id,
        document_id,
        attempt_id,
        seeded_at,
        seeded_at,
    )
    .await
}

fn two_hours_ago() -> chrono::DateTime<Utc> {
    Utc::now() - chrono::Duration::hours(2)
}

async fn load_attempt(
    pool: &PgPool,
    attempt_id: Uuid,
) -> Result<(String, Option<String>, Option<String>, bool)> {
    sqlx::query_as::<_, (String, Option<String>, Option<String>, bool)>(
        "select attempt_state::text, failure_class, failure_code, retryable
         from ingest_attempt
         where id = $1",
    )
    .bind(attempt_id)
    .fetch_one(pool)
    .await
    .context("failed to load orphan-fixture attempt")
}

async fn wait_for_attempt_terminal(
    pool: &PgPool,
    attempt_id: Uuid,
) -> Result<(String, Option<String>, Option<String>, bool)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row = load_attempt(pool, attempt_id).await?;
        if row.0 != "leased" && row.0 != "running" {
            return Ok(row);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("attempt {attempt_id} stayed {} for 10s", row.0);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn load_runtime_rows(pool: &PgPool, rows: &SeededRuntimeRows) -> Result<RuntimeRowState> {
    let extraction_status = sqlx::query_scalar::<_, String>(
        "select status from runtime_graph_extraction where id = $1",
    )
    .bind(rows.extraction_id)
    .fetch_one(pool)
    .await
    .context("failed to load runtime graph extraction status")?;
    let extraction_error = sqlx::query_scalar::<_, Option<String>>(
        "select error_message from runtime_graph_extraction where id = $1",
    )
    .bind(rows.extraction_id)
    .fetch_one(pool)
    .await
    .context("failed to load runtime graph extraction error")?;
    let execution = sqlx::query_as::<_, (String, Option<String>, Option<String>, bool)>(
        "select lifecycle_state::text, failure_code, active_stage::text, completed_at is not null
         from runtime_execution
         where id = $1",
    )
    .bind(rows.execution_id)
    .fetch_one(pool)
    .await
    .context("failed to load runtime execution state")?;
    Ok(RuntimeRowState {
        extraction_status,
        extraction_error,
        execution_state: execution.0,
        execution_failure_code: execution.1,
        active_stage: execution.2,
        completed: execution.3,
    })
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn worker_abort_guard_finalizes_attempt_and_its_runtime_rows() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let document_id =
            seed_content_document(&pool, fixture.workspace_id, fixture.library_id).await?;
        let job = seed_content_mutation_job(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            "worker-abort-guard",
        )
        .await?;
        let attempt =
            graph_reconcile_support::create_leased_graph_attempt(&pool, job.id, "worker-abort")
                .await?;
        let seeded = seed_processing_extraction_for_attempt(
            &pool,
            fixture.library_id,
            document_id,
            attempt.id,
            Utc::now(),
            Utc::now(),
        )
        .await?;

        // An armed guard dropped without a normal finalize is exactly what an
        // aborted worker task does to its future.
        let guard = worker_attempt_abort_guard_for_test(
            Arc::new(fixture.state.clone()),
            job.id,
            attempt.id,
            "extract_content".to_string(),
        );
        drop(guard);

        let (attempt_state, failure_class, failure_code, retryable) =
            wait_for_attempt_terminal(&pool, attempt.id).await?;
        assert_eq!(attempt_state, "failed");
        assert_eq!(failure_class.as_deref(), Some(WORKER_ABORTED));
        assert_eq!(failure_code.as_deref(), Some(WORKER_ABORTED));
        assert!(retryable, "worker_aborted must be retryable");

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "failed");
        assert_eq!(runtime.extraction_error.as_deref(), Some(WORKER_ABORTED));
        assert_eq!(runtime.execution_state, "failed");
        assert_eq!(runtime.execution_failure_code.as_deref(), Some(WORKER_ABORTED));
        assert!(runtime.active_stage.is_none());
        assert!(runtime.completed, "terminal execution must carry completed_at");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn worker_abort_guard_disarmed_on_normal_path_does_not_finalize() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let document_id =
            seed_content_document(&pool, fixture.workspace_id, fixture.library_id).await?;
        let job = seed_content_mutation_job(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            "worker-abort-disarm",
        )
        .await?;
        let attempt =
            graph_reconcile_support::create_leased_graph_attempt(&pool, job.id, "worker-disarm")
                .await?;
        let seeded = seed_processing_extraction_for_attempt(
            &pool,
            fixture.library_id,
            document_id,
            attempt.id,
            Utc::now(),
            Utc::now(),
        )
        .await?;

        let mut guard = worker_attempt_abort_guard_for_test(
            Arc::new(fixture.state.clone()),
            job.id,
            attempt.id,
            "extract_content".to_string(),
        );
        guard.disarm_for_test();
        drop(guard);

        // Give a wrongly-armed finalizer time to run, then prove it did not.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (attempt_state, _, failure_code, _) = load_attempt(&pool, attempt.id).await?;
        assert_eq!(attempt_state, "leased");
        assert_eq!(failure_code, None);
        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "processing");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn cancel_ingest_job_terminalizes_activated_runtime_rows_in_same_transaction() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let document_id =
            seed_content_document(&pool, fixture.workspace_id, fixture.library_id).await?;
        let job = seed_content_mutation_job(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            "cancel-chokepoint",
        )
        .await?;
        let attempt =
            graph_reconcile_support::create_leased_graph_attempt(&pool, job.id, "cancel").await?;
        // Recent rows: the reconciler's stale window cannot be the reason they
        // close, only the attempt-terminal chokepoint can.
        let seeded = seed_processing_extraction_for_attempt(
            &pool,
            fixture.library_id,
            document_id,
            attempt.id,
            Utc::now(),
            Utc::now(),
        )
        .await?;

        let affected = ingest_repository::cancel_ingest_job(&pool, job.id).await?;
        assert_eq!(affected, 1);

        let (attempt_state, _, failure_code, _) = load_attempt(&pool, attempt.id).await?;
        assert_eq!(attempt_state, "canceled");
        assert_eq!(failure_code.as_deref(), Some("canceled_by_operator"));

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "canceled");
        assert_eq!(runtime.execution_state, "canceled");
        assert_eq!(runtime.execution_failure_code.as_deref(), Some("canceled_by_operator"));
        assert!(runtime.active_stage.is_none());
        assert!(runtime.completed, "terminal execution must carry completed_at");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconciler_fails_processing_extraction_for_failed_attempt_and_is_idempotent() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let seeded = seed_reconcile_case(&fixture, Some("failed"), two_hours_ago()).await?;

        let counts = reconcile_orphan_runtime_rows_once(&pool, Duration::from_secs(60), 10).await?;
        assert_eq!(counts.extractions_terminalized, 1);
        assert_eq!(counts.executions_terminalized, 1);

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "failed");
        assert_eq!(runtime.extraction_error.as_deref(), Some(RECONCILED));
        assert_eq!(runtime.execution_state, "failed");
        assert_eq!(runtime.execution_failure_code.as_deref(), Some(RECONCILED));
        assert!(runtime.active_stage.is_none());
        assert!(runtime.completed, "terminal execution must carry completed_at");

        let second = reconcile_orphan_runtime_rows_once(&pool, Duration::from_secs(60), 10).await?;
        assert_eq!(second.extractions_terminalized, 0);
        assert_eq!(second.executions_terminalized, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconciler_cancels_processing_extraction_for_canceled_attempt() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let seeded = seed_reconcile_case(&fixture, Some("canceled"), two_hours_ago()).await?;

        let counts = reconcile_orphan_runtime_rows_once(&pool, Duration::from_secs(60), 10).await?;
        assert_eq!(counts.extractions_terminalized, 1);
        assert_eq!(counts.executions_terminalized, 1);

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "canceled");
        assert_eq!(runtime.execution_state, "canceled");
        assert_eq!(runtime.execution_failure_code.as_deref(), Some(RECONCILED));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconciler_treats_absent_activation_attempt_as_orphan() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let seeded = seed_reconcile_case(&fixture, None, two_hours_ago()).await?;

        let counts = reconcile_orphan_runtime_rows_once(&pool, Duration::from_secs(60), 10).await?;
        assert_eq!(counts.extractions_terminalized, 1);
        assert_eq!(counts.executions_terminalized, 1);

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "failed");
        assert_eq!(runtime.extraction_error.as_deref(), Some(RECONCILED));
        assert_eq!(runtime.execution_state, "failed");
        assert_eq!(runtime.execution_failure_code.as_deref(), Some(RECONCILED));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconciler_leaves_recent_rows_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let seeded = seed_reconcile_case(&fixture, Some("failed"), Utc::now()).await?;

        let counts =
            reconcile_orphan_runtime_rows_once(&pool, Duration::from_secs(3600), 10).await?;
        assert_eq!(counts.extractions_terminalized, 0);
        assert_eq!(counts.executions_terminalized, 0);

        let runtime = load_runtime_rows(&pool, &seeded).await?;
        assert_eq!(runtime.extraction_status, "processing");
        assert_eq!(runtime.execution_state, "running");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn cancel_jobs_for_document_terminalizes_mutation_items_and_operation() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let document_id =
            seed_content_document(&pool, fixture.workspace_id, fixture.library_id).await?;
        let mutation_item_id = Uuid::now_v7();
        let operation_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();

        let mutation_id = seed_accepted_document_mutation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            "replace",
        )
        .await
        .context("failed to insert cancel-disposition mutation")?;
        sqlx::query(
            "insert into content_mutation_item (id, mutation_id, document_id, item_state)
             values ($1, $2, $3, 'pending')",
        )
        .bind(mutation_item_id)
        .bind(mutation_id)
        .bind(document_id)
        .execute(&pool)
        .await
        .context("failed to insert cancel-disposition mutation item")?;
        sqlx::query(
            "insert into ops_async_operation (
                 id, workspace_id, library_id, operation_kind, surface_kind, subject_kind,
                 subject_id, status
             ) values ($1, $2, $3, 'content_mutation', 'rest', 'content_mutation', $4, 'accepted')",
        )
        .bind(operation_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(mutation_id)
        .execute(&pool)
        .await
        .context("failed to insert cancel-disposition async operation")?;
        sqlx::query(
            "insert into ingest_job (
                 id, workspace_id, library_id, mutation_id, mutation_item_id, knowledge_document_id,
                 job_kind, queue_state
             ) values ($1, $2, $3, $4, $5, $6, 'content_mutation', 'queued')",
        )
        .bind(job_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(mutation_id)
        .bind(mutation_item_id)
        .bind(document_id)
        .execute(&pool)
        .await
        .context("failed to insert cancel-disposition ingest job")?;

        let affected = ingest_repository::cancel_jobs_for_document(
            &pool,
            document_id,
            ingest_repository::CancelDisposition::DOCUMENT_DELETED,
        )
        .await?;
        assert_eq!(affected, 1);

        let job_state = sqlx::query_scalar::<_, String>(
            "select queue_state::text from ingest_job where id = $1",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .context("failed to load canceled ingest job state")?;
        assert_eq!(job_state, "canceled");

        let (mutation_state, mutation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select mutation_state::text, failure_code from content_mutation where id = $1",
            )
            .bind(mutation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load canceled content mutation")?;
        assert_eq!(mutation_state, "canceled");
        assert_eq!(mutation_failure_code.as_deref(), Some("document_deleted"));

        let item_state = sqlx::query_scalar::<_, String>(
            "select item_state::text from content_mutation_item where id = $1",
        )
        .bind(mutation_item_id)
        .fetch_one(&pool)
        .await
        .context("failed to load skipped content mutation item")?;
        assert_eq!(item_state, "skipped");

        let (operation_status, operation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select status::text, failure_code from ops_async_operation where id = $1",
            )
            .bind(operation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load terminal async operation")?;
        assert_eq!(operation_status, "failed");
        assert_eq!(operation_failure_code.as_deref(), Some("document_deleted"));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn insert_query_operation(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    subject_id: Option<Uuid>,
    status: &str,
    created_at: DateTime<Utc>,
) -> Result<Uuid> {
    let operation_id = Uuid::now_v7();
    sqlx::query(
        "insert into ops_async_operation (
             id, workspace_id, library_id, operation_kind, surface_kind, subject_kind,
             subject_id, status, created_at
         ) values (
             $1, $2, $3, 'query_execution', 'rest', 'query_execution',
             $4, $5::ops_async_operation_status, $6
         )",
    )
    .bind(operation_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(subject_id)
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await
    .context("failed to insert query_execution async operation")?;
    Ok(operation_id)
}

async fn seed_query_execution_with_runtime(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    runtime_state: &str,
    created_at: DateTime<Utc>,
) -> Result<Uuid> {
    let execution_id = Uuid::now_v7();
    let runtime_id = Uuid::now_v7();
    let conversation_id = Uuid::now_v7();
    sqlx::query(
        "insert into query_conversation (id, workspace_id, library_id) values ($1, $2, $3)",
    )
    .bind(conversation_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(pool)
    .await
    .context("failed to insert query conversation")?;
    let terminal = !matches!(runtime_state, "accepted" | "running");
    sqlx::query(
        "insert into runtime_execution (
             id, owner_kind, owner_id, task_kind, surface_kind, contract_name, contract_version,
             lifecycle_state, turn_budget, turn_count, parallel_action_limit, accepted_at,
             completed_at
         ) values (
             $1, 'query_execution', $2, 'query_answer', 'rest', 'query_answer', 'v1',
             $3::runtime_lifecycle_state, 1, 0, 1, $4, case when $5 then $4 else null end
         )",
    )
    .bind(runtime_id)
    .bind(execution_id)
    .bind(runtime_state)
    .bind(created_at)
    .bind(terminal)
    .execute(pool)
    .await
    .context("failed to insert query runtime execution")?;
    sqlx::query(
        "insert into query_execution (
             id, workspace_id, library_id, conversation_id, context_bundle_id, query_text,
             runtime_execution_id, started_at
         ) values ($1, $2, $3, $4, $5, 'seeded grounded query', $6, $7)",
    )
    .bind(execution_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(conversation_id)
    .bind(Uuid::now_v7())
    .bind(runtime_id)
    .bind(created_at)
    .execute(pool)
    .await
    .context("failed to insert query execution")?;
    Ok(execution_id)
}

async fn load_operation_state(
    pool: &PgPool,
    operation_id: Uuid,
) -> Result<(String, Option<String>, bool)> {
    sqlx::query_as::<_, (String, Option<String>, bool)>(
        "select status::text, failure_code, completed_at is not null
         from ops_async_operation
         where id = $1",
    )
    .bind(operation_id)
    .fetch_one(pool)
    .await
    .context("failed to load async operation state")
}

async fn seed_processing_query_operation(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    subject_id: Option<Uuid>,
) -> Result<Uuid> {
    insert_query_operation(
        pool,
        workspace_id,
        library_id,
        subject_id,
        "processing",
        two_hours_ago(),
    )
    .await
}

async fn reconcile_query_operations(pool: &PgPool) -> Result<u64> {
    query_repository::reconcile_orphan_query_operations_once(
        pool,
        Utc::now() - chrono::Duration::hours(1),
        100,
    )
    .await
    .context("query operation reconcile failed")
}

async fn reconcile_query_operation(
    pool: &PgPool,
    operation_id: Uuid,
    expected_updated: u64,
) -> Result<(String, Option<String>, bool)> {
    let updated = reconcile_query_operations(pool).await?;
    assert_eq!(updated, expected_updated);
    load_operation_state(pool, operation_id).await
}

async fn reconcile_content_mutations(
    pool: &PgPool,
) -> Result<content_mutation_repository::ContentMutationReconcileCounts> {
    content_mutation_repository::reconcile_orphan_content_mutations_once(
        pool,
        Utc::now() - chrono::Duration::hours(1),
        100,
    )
    .await
    .context("content mutation reconcile failed")
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_query_operations_absent_subject_becomes_failed_and_is_idempotent() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let operation_id = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(Uuid::now_v7()),
        )
        .await?;

        let (status, failure_code, completed) =
            reconcile_query_operation(&pool, operation_id, 1).await?;
        assert_eq!(status, "failed");
        assert_eq!(failure_code.as_deref(), Some(RECONCILED));
        assert!(completed, "reconciled operation must carry completed_at");

        let second = reconcile_query_operations(&pool).await?;
        assert_eq!(second, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_query_operations_completed_runtime_becomes_ready() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let execution_id = seed_query_execution_with_runtime(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            "completed",
            two_hours_ago(),
        )
        .await?;
        let operation_id = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(execution_id),
        )
        .await?;

        let (status, failure_code, completed) =
            reconcile_query_operation(&pool, operation_id, 1).await?;
        assert_eq!(status, "ready");
        assert_eq!(failure_code, None);
        assert!(completed, "reconciled operation must carry completed_at");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_query_operations_maps_canceled_and_failed_runtime() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let canceled_execution = seed_query_execution_with_runtime(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            "canceled",
            two_hours_ago(),
        )
        .await?;
        let canceled_operation = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(canceled_execution),
        )
        .await?;
        let failed_execution = seed_query_execution_with_runtime(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            "failed",
            two_hours_ago(),
        )
        .await?;
        let failed_operation = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(failed_execution),
        )
        .await?;

        let updated = reconcile_query_operations(&pool).await?;
        assert_eq!(updated, 2);

        let (canceled_status, canceled_failure, _) =
            load_operation_state(&pool, canceled_operation).await?;
        assert_eq!(canceled_status, "canceled");
        assert_eq!(canceled_failure.as_deref(), Some(RECONCILED));

        let (failed_status, failed_failure, _) =
            load_operation_state(&pool, failed_operation).await?;
        assert_eq!(failed_status, "failed");
        assert_eq!(failed_failure.as_deref(), Some(RECONCILED));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_query_operations_leaves_recent_orphan_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let operation_id = insert_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(Uuid::now_v7()),
            "processing",
            Utc::now(),
        )
        .await?;

        let updated = reconcile_query_operations(&pool).await?;
        assert_eq!(updated, 0);

        let (status, failure_code, completed) = load_operation_state(&pool, operation_id).await?;
        assert_eq!(status, "processing");
        assert_eq!(failure_code, None);
        assert!(!completed);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_query_operations_leaves_running_and_accepted_runtimes_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    // The runtimes are live (fresh `accepted_at`), so the stale-lease reaper
    // leaves them alone; only the operation row is older than the window.
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let running_execution = seed_query_execution_with_runtime(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            "running",
            Utc::now(),
        )
        .await?;
        let running_operation = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(running_execution),
        )
        .await?;
        let accepted_execution = seed_query_execution_with_runtime(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            "accepted",
            Utc::now(),
        )
        .await?;
        let accepted_operation = seed_processing_query_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Some(accepted_execution),
        )
        .await?;

        let updated = reconcile_query_operations(&pool).await?;
        assert_eq!(updated, 0, "a live runtime must not be reconciled");

        for operation_id in [running_operation, accepted_operation] {
            let (status, failure_code, completed) =
                load_operation_state(&pool, operation_id).await?;
            assert_eq!(status, "processing");
            assert_eq!(failure_code, None);
            assert!(!completed, "live runtime operation must not be completed");
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

struct SeededContentMutation {
    mutation_id: Uuid,
    item_id: Uuid,
    operation_id: Uuid,
}

async fn insert_content_mutation_operation(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    subject_id: Uuid,
    created_at: DateTime<Utc>,
) -> Result<Uuid> {
    let operation_id = Uuid::now_v7();
    sqlx::query(
        "insert into ops_async_operation (
             id, workspace_id, library_id, operation_kind, surface_kind, subject_kind,
             subject_id, status, created_at
         ) values (
             $1, $2, $3, 'content_mutation', 'rest', 'content_mutation', $4, 'accepted', $5
         )",
    )
    .bind(operation_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(subject_id)
    .bind(created_at)
    .execute(pool)
    .await
    .context("failed to insert accepted content mutation operation")?;
    Ok(operation_id)
}

/// One accepted `content_mutation` seed on a document in `document_state`
/// (`deleted_at` set when `document_deleted`), all stamped at `requested_at`.
struct AcceptedContentMutationSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    operation_kind: &'a str,
    document_state: &'a str,
    document_deleted: bool,
    requested_at: DateTime<Utc>,
}

/// Seeds an accepted `content_mutation` with one pending item on a document in
/// `document_state` (`deleted_at` set when `document_deleted`) plus its accepted
/// `ops_async_operation`, all stamped at `requested_at`.
async fn seed_accepted_content_mutation(
    pool: &PgPool,
    seed: AcceptedContentMutationSeed<'_>,
) -> Result<SeededContentMutation> {
    let document_id = Uuid::now_v7();
    sqlx::query(
        "insert into content_document (
             id, workspace_id, library_id, external_key, document_state, deleted_at
         ) values (
             $1, $2, $3, $4, $5::content_document_state, case when $6 then now() else null end
         )",
    )
    .bind(document_id)
    .bind(seed.workspace_id)
    .bind(seed.library_id)
    .bind(format!("runtime-orphan-mutation-doc-{}", Uuid::now_v7().simple()))
    .bind(seed.document_state)
    .bind(seed.document_deleted)
    .execute(pool)
    .await
    .context("failed to insert content document for mutation reconcile")?;

    let item_id = Uuid::now_v7();
    let mutation_id = seed_accepted_document_mutation(
        pool,
        seed.workspace_id,
        seed.library_id,
        document_id,
        seed.operation_kind,
    )
    .await
    .context("failed to insert accepted content mutation")?;
    sqlx::query("update content_mutation set requested_at = $2 where id = $1")
        .bind(mutation_id)
        .bind(seed.requested_at)
        .execute(pool)
        .await
        .context("failed to age accepted content mutation")?;
    sqlx::query(
        "insert into content_mutation_item (id, mutation_id, document_id, item_state)
         values ($1, $2, $3, 'pending')",
    )
    .bind(item_id)
    .bind(mutation_id)
    .bind(document_id)
    .execute(pool)
    .await
    .context("failed to insert pending content mutation item")?;
    let operation_id = insert_content_mutation_operation(
        pool,
        seed.workspace_id,
        seed.library_id,
        mutation_id,
        seed.requested_at,
    )
    .await?;
    Ok(SeededContentMutation { mutation_id, item_id, operation_id })
}

async fn seed_accepted_document_mutation(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    operation_kind: &str,
) -> Result<Uuid> {
    let identity = serde_json::json!({
        "workspaceId": workspace_id, "libraryId": library_id, "operationKind": operation_kind,
        "target": {"kind":"existing", "documentId":document_id}, "sourceIdentity":null,
    });
    Ok(crate::content_lifecycle_support::seed_pending_content_mutation(
        pool,
        crate::content_lifecycle_support::PendingContentMutationSeed {
            workspace_id,
            library_id,
            operation_kind,
            request_surface: "rest",
            mutation_state: "accepted",
            request_identity: &identity,
        },
    )
    .await?
    .id)
}

async fn insert_leased_content_mutation_job(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    mutation_id: Uuid,
    item_id: Uuid,
) -> Result<Uuid> {
    let job_id = Uuid::now_v7();
    sqlx::query(
        "insert into ingest_job (
             id, workspace_id, library_id, mutation_id, mutation_item_id, knowledge_document_id,
             job_kind, queue_state
         ) values ($1, $2, $3, $4, $5,
             (select document_id from content_mutation_item where id = $5),
             'content_mutation', 'leased')",
    )
    .bind(job_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(mutation_id)
    .bind(item_id)
    .execute(pool)
    .await
    .context("failed to insert leased content mutation job")?;
    Ok(job_id)
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_orphan_content_mutations_terminalizes_deleted_document_mutation() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let SeededContentMutation { mutation_id, item_id, operation_id } =
            seed_accepted_content_mutation(
                &pool,
                AcceptedContentMutationSeed {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "delete",
                    document_state: "deleted",
                    document_deleted: true,
                    requested_at: two_hours_ago(),
                },
            )
            .await?;

        let counts = reconcile_content_mutations(&pool).await?;
        assert_eq!(counts.mutations, 1);
        assert_eq!(counts.items, 1);
        assert_eq!(counts.operations, 1);

        let (mutation_state, mutation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select mutation_state::text, failure_code from content_mutation where id = $1",
            )
            .bind(mutation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load reconciled delete mutation")?;
        assert_eq!(mutation_state, "canceled");
        assert_eq!(mutation_failure_code.as_deref(), Some("document_deleted"));

        let item_state = sqlx::query_scalar::<_, String>(
            "select item_state::text from content_mutation_item where id = $1",
        )
        .bind(item_id)
        .fetch_one(&pool)
        .await
        .context("failed to load skipped delete mutation item")?;
        assert_eq!(item_state, "skipped");

        let (operation_status, operation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select status::text, failure_code from ops_async_operation where id = $1",
            )
            .bind(operation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load reconciled content mutation operation")?;
        assert_eq!(operation_status, "failed");
        assert_eq!(operation_failure_code.as_deref(), Some("document_deleted"));

        let second = reconcile_content_mutations(&pool).await?;
        assert_eq!(second, content_mutation_repository::ContentMutationReconcileCounts::default());
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn seed_multi_document_mutation(
    fixture: &ContentLifecycleFixture,
) -> Result<SeededContentMutation> {
    seed_accepted_content_mutation(
        &fixture.state.persistence.postgres,
        AcceptedContentMutationSeed {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            operation_kind: "replace",
            document_state: "active",
            document_deleted: false,
            requested_at: two_hours_ago(),
        },
    )
    .await
}

async fn seed_second_pending_item(
    fixture: &ContentLifecycleFixture,
    mutation_id: Uuid,
) -> Result<Uuid> {
    let pool = &fixture.state.persistence.postgres;
    let second_document_id = Uuid::now_v7();
    sqlx::query(
        "insert into content_document (
             id, workspace_id, library_id, external_key, document_state
         ) values ($1, $2, $3, $4, 'active')",
    )
    .bind(second_document_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("runtime-orphan-mutation-doc-{}", Uuid::now_v7().simple()))
    .execute(pool)
    .await
    .context("failed to insert second content document for mutation reconcile")?;
    let second_item_id = Uuid::now_v7();
    sqlx::query(
        "insert into content_mutation_item (id, mutation_id, document_id, item_state)
         values ($1, $2, $3, 'pending')",
    )
    .bind(second_item_id)
    .bind(mutation_id)
    .bind(second_document_id)
    .execute(pool)
    .await
    .context("failed to insert second pending content mutation item")?;
    Ok(second_item_id)
}

async fn assert_mutation_leaves_rows_untouched(
    pool: &PgPool,
    mutation_id: Uuid,
    item_ids: &[Uuid],
    operation_id: Uuid,
    operation_status: &str,
) -> Result<()> {
    let (mutation_state, mutation_failure_code) = sqlx::query_as::<_, (String, Option<String>)>(
        "select mutation_state::text, failure_code from content_mutation where id = $1",
    )
    .bind(mutation_id)
    .fetch_one(pool)
    .await
    .context("failed to load untouched mutation")?;
    assert_eq!(mutation_state, "accepted");
    assert_eq!(mutation_failure_code, None);
    for item_id in item_ids {
        let item_state = sqlx::query_scalar::<_, String>(
            "select item_state::text from content_mutation_item where id = $1",
        )
        .bind(item_id)
        .fetch_one(pool)
        .await
        .context("failed to load untouched mutation item")?;
        assert_eq!(item_state, "pending");
    }
    let (status, failure_code, completed) = load_operation_state(pool, operation_id).await?;
    assert_eq!(status, operation_status);
    assert_eq!(failure_code, None);
    assert!(!completed);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_content_mutations_pass4_fails_operation_with_absent_subject() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let operation_id = insert_content_mutation_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            Uuid::now_v7(),
            two_hours_ago(),
        )
        .await?;

        let counts = reconcile_content_mutations(&pool).await?;
        assert_eq!(counts.mutations, 0);
        assert_eq!(counts.items, 0);
        assert!(counts.operations >= 1, "pass 4 must fail the orphan operation");

        let (status, failure_code, completed) = load_operation_state(&pool, operation_id).await?;
        assert_eq!(status, "failed");
        assert_eq!(failure_code.as_deref(), Some(RECONCILED));
        assert!(completed, "reconciled operation must carry completed_at");

        let second = reconcile_content_mutations(&pool).await?;
        assert_eq!(second, content_mutation_repository::ContentMutationReconcileCounts::default());
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_content_mutations_leaves_mutation_with_live_job_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let SeededContentMutation { mutation_id, item_id, operation_id } =
            seed_accepted_content_mutation(
                &pool,
                AcceptedContentMutationSeed {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "replace",
                    document_state: "active",
                    document_deleted: false,
                    requested_at: two_hours_ago(),
                },
            )
            .await?;
        let _job_id = insert_leased_content_mutation_job(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            mutation_id,
            item_id,
        )
        .await?;

        let counts = reconcile_content_mutations(&pool).await?;
        assert_eq!(counts, content_mutation_repository::ContentMutationReconcileCounts::default());

        assert_mutation_leaves_rows_untouched(
            &pool,
            mutation_id,
            &[item_id],
            operation_id,
            "accepted",
        )
        .await?;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_content_mutations_leaves_multi_document_mutation_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        // A second document for the same mutation makes it span more than one
        // document; the reconciler must leave it to an explicit operator
        // decision rather than classify it from one document's outcome.
        let SeededContentMutation { mutation_id, item_id, operation_id } =
            seed_multi_document_mutation(&fixture).await?;
        let second_item_id = seed_second_pending_item(&fixture, mutation_id).await?;

        let counts = reconcile_content_mutations(&pool).await?;
        assert_eq!(counts, content_mutation_repository::ContentMutationReconcileCounts::default());

        assert_mutation_leaves_rows_untouched(
            &pool,
            mutation_id,
            &[item_id, second_item_id],
            operation_id,
            "accepted",
        )
        .await?;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn reconcile_content_mutations_terminalizes_zero_item_mutation() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let document_id =
            seed_content_document(&pool, fixture.workspace_id, fixture.library_id).await?;
        let mutation_id = seed_accepted_document_mutation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            document_id,
            "replace",
        )
        .await
        .context("failed to insert zero-item content mutation")?;
        sqlx::query("update content_mutation set requested_at = $2 where id = $1")
            .bind(mutation_id)
            .bind(two_hours_ago())
            .execute(&pool)
            .await
            .context("failed to age zero-item content mutation")?;
        let operation_id = insert_content_mutation_operation(
            &pool,
            fixture.workspace_id,
            fixture.library_id,
            mutation_id,
            two_hours_ago(),
        )
        .await?;

        let counts = reconcile_content_mutations(&pool).await?;
        assert_eq!(counts.mutations, 1);
        assert_eq!(counts.items, 0);
        assert_eq!(counts.operations, 1);

        let (mutation_state, mutation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select mutation_state::text, failure_code from content_mutation where id = $1",
            )
            .bind(mutation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load reconciled zero-item mutation")?;
        assert_eq!(mutation_state, "failed");
        assert_eq!(mutation_failure_code.as_deref(), Some(RECONCILED));

        let (operation_status, operation_failure_code) =
            sqlx::query_as::<_, (String, Option<String>)>(
                "select status::text, failure_code from ops_async_operation where id = $1",
            )
            .bind(operation_id)
            .fetch_one(&pool)
            .await
            .context("failed to load reconciled zero-item mutation operation")?;
        assert_eq!(operation_status, "failed");
        assert_eq!(operation_failure_code.as_deref(), Some(RECONCILED));

        let second = reconcile_content_mutations(&pool).await?;
        assert_eq!(second, content_mutation_repository::ContentMutationReconcileCounts::default());
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
