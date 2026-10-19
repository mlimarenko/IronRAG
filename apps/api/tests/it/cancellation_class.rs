//! IRG-203 SITE-2 / SITE-4 cancellation-class integration coverage.
//!
//! These tests need a live Postgres with the required extensions, so they are
//! `#[ignore]`d and run only in the integration lane. They exercise the
//! detached reset+admit sequence and the detached fan-outs that the HTTP
//! handlers now use, proving that a caller abort detaches the work instead of
//! cancelling it mid-mutation.
//!
//! The suite is compiled only under `--features test-support` because it calls
//! the test seams (`reprocess_single_document_after_reset_cancel_for_test`,
//! `one_shot_pause_probe`).

use std::{error::Error, time::Duration};

use anyhow::Context;
use axum::{Json, extract::State};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use ironrag_backend::{
    domains::iam::PrincipalKind,
    infra::repositories::{
        iam_repository::{self, SystemRole},
        runtime_graph_repository,
    },
    interfaces::http::{
        auth::{AuthContext, AuthTokenKind},
        content::batch::{
            BatchCancelRequest, BatchDocumentOperation, batch_cancel_documents,
            reprocess_single_document, reprocess_single_document_after_reset_cancel_for_test,
            spawn_batch_delete_worker_for_test,
        },
        ops::{BulkIngestQueueActionRequest, IngestQueueBulkAction, bulk_ingest_queue_action},
    },
    services::content::{
        service::{
            CreateDocumentCommand, CreateRevisionCommand, PromoteHeadCommand,
            UploadInlineDocumentCommand,
        },
        storage::ContentStorageService,
    },
    shared::cancellation::{graph_convergence_fault, one_shot_pause_probe},
};

use crate::content_lifecycle_support::ContentLifecycleFixture;

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn suppressed_historical_sources_are_not_downloadable() -> anyhow::Result<()> {
    use ironrag_backend::interfaces::http::content::source_download::{
        SourceDownloadQuery, SourceRepresentation, download_document_source,
    };
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document_id = create_reprocessable_document(&fixture).await?;
        let old_revision_id = fixture
            .state
            .application_services
            .content
            .get_document_head(&fixture.state, document_id)
            .await?
            .and_then(|head| head.readable_revision_id)
            .context("base head missing")?;
        let principal_id = create_reprocess_principal(&fixture).await?;
        let mut auth = admin_auth(fixture.workspace_id);
        auth.principal_id = principal_id;
        let readable_response = Box::pin(download_document_source(
            auth.clone(),
            State(fixture.state.clone()),
            axum::extract::Path(document_id),
            axum::extract::Query(SourceDownloadQuery {
                revision_id: Some(old_revision_id),
                representation: None,
            }),
        ))
        .await?;
        assert_eq!(readable_response.status(), axum::http::StatusCode::OK);
        accept_document_visibility_intent(&fixture, document_id, principal_id).await?;
        for representation in [None, Some(SourceRepresentation::EditorMarkdown)] {
            let response = Box::pin(download_document_source(
                auth.clone(),
                State(fixture.state.clone()),
                axum::extract::Path(document_id),
                axum::extract::Query(SourceDownloadQuery {
                    revision_id: Some(old_revision_id),
                    representation,
                }),
            ))
            .await;
            use axum::response::IntoResponse;
            let error = response.expect_err("suppressed historical bytes must not be downloadable");
            assert_eq!(error.into_response().status(), axum::http::StatusCode::NOT_FOUND);
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn protected_reprocess_http_cannot_release_independent_intent() -> anyhow::Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        for idempotency_key in [None, Some("protected-retry")] {
            let document_id = create_reprocessable_document(&fixture).await?;
            let mut auth = admin_auth(fixture.workspace_id);
            auth.principal_id = create_reprocess_principal(&fixture).await?;
            let intent_id = accept_document_visibility_intent(&fixture, document_id, auth.principal_id).await?;
            let before: i64 = sqlx::query_scalar("select count(*) from content_mutation")
                .fetch_one(&fixture.state.persistence.postgres).await?;
            let payload = serde_json::from_value(serde_json::json!({"idempotencyKey": idempotency_key}))?;
            let error =
                Box::pin(ironrag_backend::interfaces::http::content::reprocess_document(
                    auth,
                    State(fixture.state.clone()),
                    axum::extract::Path(document_id),
                    Json(payload),
                ))
                .await.expect_err("reprocess cannot claim a protected replacement without its intent binding");
            assert!(matches!(error, ironrag_backend::interfaces::http::router_support::ApiError::VisibilityIntentRequired(_)));
            let (current_intent, readable): (Uuid, Option<Uuid>) = sqlx::query_as(
                "select h.visibility_intent_id, h.readable_revision_id
                  from content_document_head h
                  where h.document_id = $1",
            )
            .bind(document_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
            assert_eq!(current_intent, intent_id, "reprocess must retain the independent obligation");
            assert_eq!(readable, None);
            let after: i64 = sqlx::query_scalar("select count(*) from content_mutation")
                .fetch_one(&fixture.state.persistence.postgres).await?;
            assert_eq!(after, before, "rejected protected reprocess admits no payload mutation");
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn accept_document_visibility_intent(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
    principal_id: Uuid,
) -> anyhow::Result<Uuid> {
    use ironrag_backend::infra::repositories::admission_repository::{
        VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint,
    };
    let request = VisibilityAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        document_id,
        requested_by_principal_id: Some(principal_id),
        request_surface: "rest".to_owned(),
        intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
            idempotency_key: format!("observed-denial-{document_id}"),
            source_identity: "cancellation-source".to_owned(),
            materialization_token: None,
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                checksum: format!("sha256:{}", "a".repeat(64)),
                byte_size: 0,
                mime_type: "text/plain".to_owned(),
                file_name: "safe-replacement.txt".to_owned(),
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        },
    };
    Ok(admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &request, None)
        .await?
        .into_bundle()
        .receipt
        .visibility_intent_id)
}

/// A system administrator sufficient for both `POLICY_USAGE_READ` and the
/// library/document write policies the handlers check.
fn admin_auth(workspace_id: Uuid) -> AuthContext {
    AuthContext {
        token_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        parent_principal_id: None,
        workspace_id: Some(workspace_id),
        token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
        scopes: vec!["workspace_admin".to_string()],
        grants: Vec::new(),
        workspace_memberships: Vec::new(),
        visible_workspace_ids: [workspace_id].into_iter().collect(),
        is_system_admin: true,
        system_role: Some(SystemRole::Admin),
    }
}

/// Creates an uploaded document with a persisted source blob and a readable
/// base revision, so `reprocess_single_document` can resolve a reprocess source.
async fn create_reprocessable_document(fixture: &ContentLifecycleFixture) -> anyhow::Result<Uuid> {
    let workspace_id = fixture.workspace_id;
    let library_id = fixture.library_id;
    let document = fixture
        .state
        .application_services
        .content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id,
                library_id,
                external_key: Some(format!("irg203-cancel-{}", Uuid::now_v7())),
                file_name: Some("reprocess-base.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await
        .context("failed to create the reprocess base document")?;
    let document_id = document.id;

    let base_text = "line one\nline two\n";
    let base_checksum = format!("sha256:{}", hex::encode(Sha256::digest(base_text.as_bytes())));
    let storage_key = fixture
        .state
        .content_storage
        .persist_revision_source(
            workspace_id,
            library_id,
            "reprocess-base.txt",
            &base_checksum,
            base_text.as_bytes(),
        )
        .await
        .context("failed to persist the reprocess base source bytes")?;
    let base_revision = crate::content_lifecycle_support::create_imported_revision(
        &fixture.state,
        CreateRevisionCommand {
            document_id,
            content_source_kind: "upload".to_string(),
            checksum: base_checksum,
            mime_type: "text/plain".to_string(),
            byte_size: i64::try_from(base_text.len()).unwrap_or(i64::MAX),
            title: Some("reprocess-base.txt".to_string()),
            language_code: None,
            source_uri: Some("upload://reprocess-base.txt".to_string()),
            document_hint: None,
            storage_key: Some(storage_key),
            created_by_principal_id: None,
        },
        "reprocess-base.txt",
    )
    .await
    .context("failed to create the reprocess base revision")?;
    fixture
        .state
        .application_services
        .knowledge
        .set_revision_extract_state(
            &fixture.state,
            base_revision.id,
            "ready",
            Some(base_text),
            Some("sha256:irg203-base"),
        )
        .await
        .context("failed to make the reprocess base revision readable")?;
    fixture
        .state
        .application_services
        .content
        .promote_document_head(
            &fixture.state,
            PromoteHeadCommand {
                publishing_mutation_item_id: crate::content_lifecycle_support::publication_item(
                    &fixture.state.persistence.postgres,
                    base_revision.id,
                    "reprocess-base.txt",
                )
                .await?,
                document_id,
                active_revision_id: Some(base_revision.id),
                readable_revision_id: Some(base_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to promote the reprocess base head")?;
    Ok(document_id)
}

/// Creates the `iam_principal` the reprocess admissions must reference.
///
/// The admission persists `content_mutation.requested_by_principal_id`, which
/// is `content_mutation_requested_by_principal_id_fkey` -> `iam_principal(id)`.
/// A synthetic id fails admission with a foreign-key violation
/// (`map_admission_database_error` -> "references a resource that no longer
/// exists") before any mutation exists.
async fn create_reprocess_principal(fixture: &ContentLifecycleFixture) -> anyhow::Result<Uuid> {
    let principal = iam_repository::create_principal(
        &fixture.state.persistence.postgres,
        "user",
        "IRG-203 cancellation principal",
        None,
    )
    .await
    .context("failed to create the reprocess principal")?;
    Ok(principal.id)
}

/// Counts the non-terminal leftovers a caller abort could strand: head
/// mutations still accepted/running, jobs still queued/leased/paused, and
/// attempts still leased/running.
async fn non_terminal_leftovers(
    pool: &PgPool,
    document_id: Uuid,
) -> anyhow::Result<(i64, i64, i64)> {
    let mutations = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from content_mutation m
         join content_mutation_item mi on mi.mutation_id = m.id
         where mi.document_id = $1
           and m.mutation_state in ('accepted', 'running')",
    )
    .bind(document_id)
    .fetch_one(pool)
    .await
    .context("failed to count non-terminal mutations")?;
    let jobs = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_job
         where knowledge_document_id = $1
           and queue_state in ('queued', 'leased', 'paused')",
    )
    .bind(document_id)
    .fetch_one(pool)
    .await
    .context("failed to count non-terminal jobs")?;
    let attempts = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_attempt a
         join ingest_job j on j.id = a.job_id
         where j.knowledge_document_id = $1
           and a.attempt_state in ('leased', 'running')",
    )
    .bind(document_id)
    .fetch_one(pool)
    .await
    .context("failed to count non-terminal attempts")?;
    Ok((mutations, jobs, attempts))
}

async fn mutation_state(pool: &PgPool, mutation_id: Uuid) -> anyhow::Result<String> {
    sqlx::query_scalar::<_, String>(
        "select mutation_state::text from content_mutation where id = $1",
    )
    .bind(mutation_id)
    .fetch_one(pool)
    .await
    .context("failed to read the mutation state")
}

fn reset_and_admission_finished(original_state: &str, replacement_admitted: bool) -> bool {
    original_state != "accepted" && original_state != "running" && replacement_admitted
}

#[test]
fn reset_completion_requires_the_replacement_admission() {
    for (state, admitted, expected) in [
        ("accepted", false, false),
        ("accepted", true, false),
        ("running", true, false),
        ("failed", false, false),
        ("failed", true, true),
        ("canceled", true, true),
        ("applied", true, true),
    ] {
        assert_eq!(
            reset_and_admission_finished(state, admitted),
            expected,
            "original={state}, replacement_admitted={admitted}",
        );
    }
}

/// Reset reconciliation commits before the detached task admits its replacement.
/// Read both postconditions in one snapshot; the first commit alone is not completion.
async fn wait_for_reset_and_admission(
    pool: &PgPool,
    mutation_id: Uuid,
    document_id: Uuid,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (state, admitted) = sqlx::query_as::<_, (String, bool)>(
            "select original.mutation_state::text,
                    exists (
                        select 1
                        from content_mutation replacement
                        join content_mutation_item item on item.mutation_id = replacement.id
                        where item.document_id = $2
                          and replacement.id <> original.id
                          and replacement.mutation_state in ('accepted', 'running')
                    )
             from content_mutation original
             where original.id = $1",
        )
        .bind(mutation_id)
        .bind(document_id)
        .fetch_one(pool)
        .await
        .context("failed to read the reset and replacement admission state")?;
        if reset_and_admission_finished(&state, admitted) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "reprocess did not complete: original mutation {mutation_id} is {state}, replacement_admitted={admitted}"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_every_job_state(
    pool: &PgPool,
    job_ids: &[Uuid],
    expected: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut observed = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let state = sqlx::query_scalar::<_, String>(
                "select queue_state::text from ingest_job where id = $1",
            )
            .bind(job_id)
            .fetch_one(pool)
            .await
            .context("failed to read a job queue_state")?;
            observed.push(state);
        }
        if observed.iter().all(|state| state == expected) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("jobs did not all reach {expected}; last observed: {observed:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Bounded wait for every ingest job that belongs to one document to reach
/// `expected`. Used by the batch-cancel test, which does not know the job ids
/// upfront (the reprocess admission creates them).
async fn wait_for_document_jobs_state(
    pool: &PgPool,
    document_id: Uuid,
    expected: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let observed: Vec<String> = sqlx::query_scalar(
            "select queue_state::text from ingest_job where knowledge_document_id = $1",
        )
        .bind(document_id)
        .fetch_all(pool)
        .await
        .context("failed to read the document's job queue states")?;
        if !observed.is_empty() && observed.iter().all(|state| state == expected) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "document {document_id} jobs did not all reach {expected}; last observed: {observed:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Counts the non-terminal tracking parents of one batch endpoint in a
/// workspace. Scoped to `parent_async_operation_id IS NULL` and the endpoint's
/// `operation_kind` so the child `content_mutation` operations the reprocess
/// admission legitimately leaves `accepted` (no ingest worker runs in this
/// lane) are not miscounted.
async fn non_terminal_parent_ops_count(
    pool: &PgPool,
    workspace_id: Uuid,
    operation_kind: &str,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ops_async_operation
         where workspace_id = $1
           and operation_kind = $2
           and parent_async_operation_id is null
           and status in ('accepted', 'processing')",
    )
    .bind(workspace_id)
    .bind(operation_kind)
    .fetch_one(pool)
    .await
    .context("failed to count non-terminal parent operations")
}

/// Bounded wait until the endpoint's tracking parent has reached a terminal
/// state. Asserting immediately after the fan-out would race the finalize.
async fn wait_for_no_non_terminal_parent_ops(
    pool: &PgPool,
    workspace_id: Uuid,
    operation_kind: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if non_terminal_parent_ops_count(pool, workspace_id, operation_kind).await? == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let left = non_terminal_parent_ops_count(pool, workspace_id, operation_kind).await?;
            anyhow::bail!(
                "{operation_kind} left {left} non-terminal parent operation(s) after the caller abort"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// IRG-203 SITE-2: aborting the reprocess caller between
/// `cancel_jobs_for_document` and its reconciliation must not leave the head
/// mutation accepted/running with its jobs already canceled. The detached
/// reset+admit sequence completes instead.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn aborted_reprocess_leaves_no_non_terminal_state() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result =
        async {
            let document_id = create_reprocessable_document(&fixture).await?;
            let principal_id = create_reprocess_principal(&fixture).await?;

            // First admission creates the in-flight accepted mutation + queued job
            // the abort will find. No worker runs in this lane, so it stays inflight.
            let in_flight =
                reprocess_single_document(&fixture.state, None, principal_id, None, document_id)
                    .await
                    .context("failed to seed the in-flight reprocess mutation")?;
            let original_mutation_id = in_flight.mutation.id;

            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

            let caller = tokio::spawn({
                let state = fixture.state.clone();
                async move {
                    reprocess_single_document_after_reset_cancel_for_test(
                        &state,
                        None,
                        principal_id,
                        None,
                        document_id,
                        async move {
                            let _ = reached_tx.send(());
                            let _ = release_rx.await;
                        },
                    )
                    .await
                }
            });

            reached_rx.await.context("the reset paused before its reconcile never signalled")?;
            caller.abort();
            let cancelled = caller.await;
            assert!(cancelled.is_err(), "the caller task must be cancelled, not completed");
            let _ = release_tx.send(());

            let pool = fixture.state.persistence.postgres.clone();
            wait_for_reset_and_admission(
                &pool,
                original_mutation_id,
                document_id,
                Duration::from_secs(60),
            )
            .await
            .context("the detached reset and replacement admission did not complete")?;

            let (mutations, _jobs, attempts) = non_terminal_leftovers(&pool, document_id).await?;
            assert_eq!(attempts, 0, "a canceled reset must not leave a leased/running attempt");
            // The retry's own admission legitimately leaves one accepted mutation
            // (and its queued job) until the ingest worker runs, which this lane does
            // not start; the original in-flight mutation must be the terminal one.
            assert!(
                mutations >= 1,
                "the retry must admit a new mutation after resetting the original",
            );
            assert_eq!(
                mutation_state(&pool, original_mutation_id).await?,
                "failed",
                "the aborted reprocess must reconcile the original mutation to a terminal state",
            );
            Ok(())
        }
        .await;

    fixture.cleanup().await?;
    result
}

/// IRG-203 SITE-4: aborting the bulk-action caller after the first job must not
/// stop the detached fan-out; every selected job reaches the requested state and
/// the tracking parent operation is finalized.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn aborted_bulk_queue_action_still_applies_every_job()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let mut job_ids = Vec::new();
        for _ in 0..3 {
            let job_id = sqlx::query_scalar::<_, Uuid>(
                "insert into ingest_job (workspace_id, library_id, job_kind, queue_state, priority)
                 values ($1, $2, 'reindex', 'queued', 100)
                 returning id",
            )
            .bind(fixture.workspace_id)
            .bind(fixture.library_id)
            .fetch_one(&pool)
            .await
            .context("failed to seed a queued ingest job")?;
            job_ids.push(job_id);
        }

        let probe = one_shot_pause_probe::install();
        let auth = admin_auth(fixture.workspace_id);
        let caller = tokio::spawn({
            let state = fixture.state.clone();
            let job_ids = job_ids.clone();
            async move {
                bulk_ingest_queue_action(
                    auth,
                    State(state),
                    Json(BulkIngestQueueActionRequest {
                        action: IngestQueueBulkAction::Cancel,
                        job_ids,
                    }),
                )
                .await
            }
        });

        probe.reached.await.context("the detached fan-out never reached its first-item pause")?;
        caller.abort();
        let cancelled = caller.await;
        assert!(cancelled.is_err(), "the caller task must be cancelled, not completed");
        let _ = probe.release.send(());

        wait_for_every_job_state(&pool, &job_ids, "canceled", Duration::from_secs(60))
            .await
            .context("the detached fan-out did not cancel every selected job")?;
        wait_for_no_non_terminal_parent_ops(
            &pool,
            fixture.workspace_id,
            "bulk_ingest_queue_action",
            Duration::from_secs(60),
        )
        .await
        .context("the detached bulk fan-out left a non-terminal parent operation")?;
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-203 SITE-4: aborting the batch-cancel caller after the first document
/// must not stop the detached fan-out; every selected document's jobs reach
/// `canceled` and the tracking parent operation is finalized.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn aborted_batch_cancel_still_cancels_every_document()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let principal_id = create_reprocess_principal(&fixture).await?;
        let mut document_ids = Vec::new();
        for _ in 0..3 {
            let document_id = create_reprocessable_document(&fixture).await?;
            // One in-flight reprocess per document gives each a queued job for
            // the cancel to find. No worker runs in this lane, so it stays queued.
            reprocess_single_document(&fixture.state, None, principal_id, None, document_id)
                .await
                .context("failed to seed the in-flight reprocess mutation")?;
            document_ids.push(document_id);
        }

        let probe = one_shot_pause_probe::install();
        let auth = admin_auth(fixture.workspace_id);
        let caller = tokio::spawn({
            let state = fixture.state.clone();
            let document_ids = document_ids.clone();
            async move {
                batch_cancel_documents(
                    auth,
                    State(state),
                    Json(BatchCancelRequest { document_ids }),
                )
                .await
            }
        });

        probe
            .reached
            .await
            .context("the detached cancel fan-out never reached its first-item pause")?;
        caller.abort();
        let cancelled = caller.await;
        assert!(cancelled.is_err(), "the caller task must be cancelled, not completed");
        let _ = probe.release.send(());

        for document_id in &document_ids {
            wait_for_document_jobs_state(&pool, *document_id, "canceled", Duration::from_secs(60))
                .await
                .context("the detached fan-out did not cancel every selected document")?;
        }
        wait_for_no_non_terminal_parent_ops(
            &pool,
            fixture.workspace_id,
            "batch_cancel_documents",
            Duration::from_secs(60),
        )
        .await
        .context("the detached batch cancel left a non-terminal parent operation")?;
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Creates `count` documents a batch delete can remove. When
/// `with_graph_evidence` is set, each also gets a runtime graph node so the
/// delete runs its graph-convergence step.
async fn create_batch_delete_documents(
    fixture: &ContentLifecycleFixture,
    count: usize,
    with_graph_evidence: bool,
) -> anyhow::Result<Vec<Uuid>> {
    let mut document_ids = Vec::new();
    for _ in 0..count {
        let document_id = create_reprocessable_document(fixture).await?;
        if with_graph_evidence {
            runtime_graph_repository::upsert_runtime_graph_document_node(
                &fixture.state.persistence.postgres,
                runtime_graph_repository::RuntimeGraphDocumentNodeInput {
                    library_id: fixture.library_id,
                    document_id,
                    identity_key: &format!("document:{document_id}"),
                    label: "Synthetic document",
                    aliases_json: serde_json::json!([]),
                    summary: None,
                    metadata_json: serde_json::json!({}),
                    support_count: 1,
                    projection_version: 1,
                },
            )
            .await
            .context("failed to seed a document graph node")?;
        }
        document_ids.push(document_id);
    }
    Ok(document_ids)
}

/// Plants the `batch_delete_documents` parent in `processing` — the row the
/// HTTP handler creates before spawning its worker.
async fn plant_batch_delete_parent(fixture: &ContentLifecycleFixture) -> anyhow::Result<Uuid> {
    let parent_id = Uuid::now_v7();
    sqlx::query(
        "insert into ops_async_operation (
             id, workspace_id, library_id, operation_kind, surface_kind,
             status, subject_kind, subject_id
         ) values (
             $1, $2, $3, 'batch_delete_documents', 'rest',
             'processing'::ops_async_operation_status, 'library', $3
         )",
    )
    .bind(parent_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to plant the batch-delete parent operation")?;
    Ok(parent_id)
}

/// IRG-218: a detached batch-delete worker aborted before it settles its parent
/// — the runtime dropping it on a rolling deployment — must still leave the
/// fan-out parent terminal. The abnormal-exit guard derives the parent from its
/// persisted children or force-fails it, so a poller never sees a hung
/// operation. Before IRG-218 the worker was a bare `tokio::spawn` with no
/// finalizer, and the parent stayed `processing` forever.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn aborted_batch_delete_worker_settles_parent() -> Result<(), Box<dyn Error + Send + Sync>> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let principal_id = create_reprocess_principal(&fixture).await?;
        let document_ids = create_batch_delete_documents(&fixture, 3, false).await?;
        let parent_id = plant_batch_delete_parent(&fixture).await?;

        let probe = one_shot_pause_probe::install();
        let worker = spawn_batch_delete_worker_for_test(
            fixture.state.clone(),
            BatchDocumentOperation {
                parent_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                principal_id,
                document_ids: document_ids.clone(),
                parallelism: 3,
            },
        );

        probe
            .reached
            .await
            .context("the detached batch-delete fan-out never reached its first-child pause")?;
        worker.abort();
        let aborted = worker.await;
        assert!(aborted.is_err(), "the worker must be aborted, not completed");
        let _ = probe.release.send(());

        wait_for_no_non_terminal_parent_ops(
            &pool,
            fixture.workspace_id,
            "batch_delete_documents",
            Duration::from_secs(60),
        )
        .await
        .context("the aborted batch-delete worker left a non-terminal parent operation")?;

        let (status, failure_code) = sqlx::query_as::<_, (String, Option<String>)>(
            "select status::text, failure_code from ops_async_operation where id = $1",
        )
        .bind(parent_id)
        .fetch_one(&pool)
        .await
        .context("failed to read the settled parent operation")?;
        assert_eq!(
            status, "failed",
            "an aborted worker must force-fail a parent that still has in-flight children",
        );
        assert_eq!(
            failure_code.as_deref(),
            Some("batch_worker_aborted"),
            "the guard must record why it force-failed the parent",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-218 (second symptom): a background graph-projection failure after the
/// documents were deleted must not mark the parent failed. The parent's outcome
/// is its children's (`ready`); the projection failure is recorded as a
/// `graph_convergence` detail. Before IRG-218 the worker wrote
/// `failed/graph_projection_failed` directly and bypassed the aggregate.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn batch_delete_projection_failure_keeps_parent_ready()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let pool = fixture.state.persistence.postgres.clone();
        let principal_id = create_reprocess_principal(&fixture).await?;
        let document_ids = create_batch_delete_documents(&fixture, 2, true).await?;
        let parent_id = plant_batch_delete_parent(&fixture).await?;

        graph_convergence_fault::install();
        let worker = spawn_batch_delete_worker_for_test(
            fixture.state.clone(),
            BatchDocumentOperation {
                parent_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                principal_id,
                document_ids: document_ids.clone(),
                parallelism: 1,
            },
        );

        tokio::time::timeout(Duration::from_secs(60), worker)
            .await
            .context("the batch-delete worker did not finish")?
            .context("the batch-delete worker panicked")?;

        let (status, failure_code, detail) = sqlx::query_as::<
            _,
            (String, Option<String>, Option<serde_json::Value>),
        >(
            "select status::text, failure_code, result_json from ops_async_operation where id = $1",
        )
        .bind(parent_id)
        .fetch_one(&pool)
        .await
        .context("failed to read the settled parent operation")?;
        let child_states: Vec<(String, i64)> = sqlx::query_as(
            "select status::text, count(*)::bigint
               from ops_async_operation
              where parent_async_operation_id = $1
              group by 1 order by 1",
        )
        .bind(parent_id)
        .fetch_all(&pool)
        .await
        .context("failed to read the child states")?;
        assert_eq!(
            status, "ready",
            "a background projection failure must not fail the deletion; \
             failure_code={failure_code:?} children={child_states:?} detail={detail:?}",
        );
        assert_eq!(failure_code, None, "the parent's outcome is its children's");
        assert_eq!(
            detail
                .as_ref()
                .and_then(|value| value.pointer("/graph_convergence/failure_code"))
                .and_then(|value| value.as_str()),
            Some("graph_projection_failed"),
            "the projection failure is recorded as a detail",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Bounded wait until content storage no longer holds `storage_key`.
///
/// SITE-6's guard reclaims on an abnormal drop by spawning the reclaimer, so the
/// object disappears asynchronously; the test polls rather than asserting
/// immediately.
async fn wait_for_revision_source_absent(
    storage: &ContentStorageService,
    storage_key: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let present = storage
            .has_revision_source(storage_key)
            .await
            .context("failed to probe the staged revision source")?;
        if !present {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the caller abort left {storage_key} staged with no committed revision: the guard did not reclaim it"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// IRG-203 SITE-6: the inline upload writes the content-addressed source blob
/// *before* the admission transaction commits a revision that references it. A
/// caller aborted in that window — a client disconnect, a proxy abandoning the
/// request — must not leave the blob orphaned: the armed
/// `PersistedRevisionSourceGuard` reclaims it on drop, and no committed
/// revision references the key.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn aborted_inline_upload_reclaims_staged_source_blob()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let workspace_id = fixture.workspace_id;
        let library_id = fixture.library_id;
        let file_name = "irg203-site6-staged.txt";
        let payload = b"IRG-203 SITE-6 staged source bytes\n";
        let checksum = format!("sha256:{}", hex::encode(Sha256::digest(payload)));
        let storage_key = ContentStorageService::build_revision_storage_key(
            workspace_id,
            library_id,
            file_name,
            &checksum,
        );

        // Park the upload between its blob write and admission, then abort the
        // caller exactly in that window.
        let probe = one_shot_pause_probe::install();
        let caller = tokio::spawn({
            let state = fixture.state.clone();
            async move {
                state
                    .application_services
                    .content
                    .upload_inline_document(
                        &state,
                        UploadInlineDocumentCommand {
                            workspace_id,
                            library_id,
                            external_key: Some(format!("irg203-site6-{}", Uuid::now_v7())),
                            idempotency_key: None,
                            requested_by_principal_id: None,
                            request_surface: "rest".to_string(),
                            source_identity: None,
                            file_name: file_name.to_string(),
                            title: Some(file_name.to_string()),
                            document_hint: None,
                            mime_type: Some("text/plain".to_string()),
                            file_bytes: payload.to_vec(),
                            parent_external_key: None,
                        },
                    )
                    .await
            }
        });

        probe
            .reached
            .await
            .context("the inline upload never paused between its blob write and admission")?;
        // The probe fires after the persist, so the staged blob must exist now.
        assert!(
            fixture
                .state
                .content_storage
                .has_revision_source(&storage_key)
                .await
                .context("failed to probe the staged blob after the write")?,
            "the staged blob must be present between the write and admission",
        );

        caller.abort();
        let cancelled = caller.await;
        assert!(cancelled.is_err(), "the caller task must be cancelled, not completed");
        drop(probe.release);

        // The armed guard reclaims asynchronously on drop; bounded wait.
        wait_for_revision_source_absent(
            &fixture.state.content_storage,
            &storage_key,
            Duration::from_secs(30),
        )
        .await?;

        let referencing = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from content_revision where storage_key = $1",
        )
        .bind(&storage_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count revisions referencing the staged key")?;
        assert_eq!(
            referencing, 0,
            "the aborted upload must not have committed a revision referencing the staged key",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
