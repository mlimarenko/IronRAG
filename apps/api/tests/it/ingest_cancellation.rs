use std::{error::Error, time::Duration};

use anyhow::Context;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use ironrag_backend::{
    app::seed_clean_v1_ai_catalog,
    services::{
        content::service::{
            AppendInlineMutationCommand, CreateDocumentCommand, CreateRevisionCommand,
            PromoteHeadCommand,
        },
        ingest::cancellation::{StageError, StageResult, ensure_not_cancelled},
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    content_lifecycle_support::ContentLifecycleFixture,
    ingest_ai_support::{IngestAiFixtureConfig, configure_ingest_ai_fixture},
};

async fn fake_long_running_stage(cancellation_token: CancellationToken) -> StageResult<()> {
    loop {
        ensure_not_cancelled(&cancellation_token)?;
        tokio::select! {
            () = cancellation_token.cancelled() => return Err(StageError::Cancelled),
            () = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
    }
}

#[tokio::test]
async fn fake_long_running_stage_returns_cancelled_quickly()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let cancellation_token = CancellationToken::new();
    let task_token = cancellation_token.clone();
    let stage = tokio::spawn(async move { fake_long_running_stage(task_token).await });

    tokio::time::sleep(Duration::from_millis(10)).await;
    let started_at = std::time::Instant::now();
    cancellation_token.cancel();

    let stage_result = tokio::time::timeout(Duration::from_millis(200), stage).await??;

    assert_eq!(stage_result, Err(StageError::Cancelled));
    assert!(started_at.elapsed() < Duration::from_millis(200));
    Ok(())
}

const GRAPH_OUTPUT: &str = r#"{"entities":[{"label":"Seed","node_type":"artifact","aliases":[],"sub_type":null,"summary":""},{"label":"append","node_type":"process","aliases":[],"sub_type":null,"summary":""}],"relations":[{"source_label":"Seed","target_label":"append","relation_type":"describes","summary":""}]}"#;

async fn latest_attempt(
    pool: &PgPool,
    document_id: Uuid,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    Ok(sqlx::query_as::<_, (String, Option<String>)>(
        "select a.attempt_state::text, a.failure_code
         from ingest_attempt a
         join ingest_job j on j.id = a.job_id
         where j.knowledge_document_id = $1
         order by a.started_at desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await?)
}

async fn wait_for_attempt_state(
    pool: &PgPool,
    document_id: Uuid,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> anyhow::Result<(String, Option<String>)> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some((state, failure_code)) = latest_attempt(pool, document_id).await?
            && predicate(&state)
        {
            return Ok((state, failure_code));
        }
        if tokio::time::Instant::now() >= deadline {
            let last = latest_attempt(pool, document_id).await?;
            anyhow::bail!(
                "timed out waiting for the ingest attempt; last observed state: {last:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Creates a text document whose head is a readable revision with stored source
/// bytes, so an inline append can load and merge them.
async fn create_appendable_document(fixture: &ContentLifecycleFixture) -> anyhow::Result<Uuid> {
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
                external_key: Some(format!("irg199-cancel-{}", Uuid::now_v7())),
                file_name: Some("append-base.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await
        .context("failed to create the append base document")?;
    let document_id = document.id;

    let base_text = "line one\nline two\n";
    let base_checksum = format!("sha256:{}", hex::encode(Sha256::digest(base_text.as_bytes())));
    let storage_key = fixture
        .state
        .content_storage
        .persist_revision_source(
            workspace_id,
            library_id,
            "append-base.txt",
            &base_checksum,
            base_text.as_bytes(),
        )
        .await
        .context("failed to persist the append base source bytes")?;
    let base_revision = crate::content_lifecycle_support::create_imported_revision(
        &fixture.state,
        CreateRevisionCommand {
            document_id,
            content_source_kind: "upload".to_string(),
            checksum: base_checksum,
            mime_type: "text/plain".to_string(),
            byte_size: i64::try_from(base_text.len()).unwrap_or(i64::MAX),
            title: Some("append-base.txt".to_string()),
            language_code: None,
            source_uri: Some("upload://append-base.txt".to_string()),
            document_hint: None,
            storage_key: Some(storage_key),
            created_by_principal_id: None,
        },
        "append-base.txt",
    )
    .await
    .context("failed to create the append base revision")?;
    fixture
        .state
        .application_services
        .knowledge
        .set_revision_extract_state(
            &fixture.state,
            base_revision.id,
            "ready",
            Some(base_text),
            Some("sha256:irg199-base"),
        )
        .await
        .context("failed to make the append base revision readable")?;
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
                    "append-base.txt",
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
        .context("failed to promote the append base head")?;
    Ok(document_id)
}

/// Spawns an inline append exactly as a handler would, waits until its attempt
/// is leased, then drops the caller exactly as a client disconnect does. The
/// shared front half of both cancellation tests.
async fn admit_then_abandon_append(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
) -> anyhow::Result<()> {
    let pool = fixture.state.persistence.postgres.clone();
    let state_for_task = fixture.state.clone();
    let base_revision_id: Uuid = sqlx::query_scalar(
        "select id from content_revision where document_id=$1 and content_source_kind='upload'",
    )
    .bind(document_id)
    .fetch_one(&pool)
    .await?;
    let service = fixture.state.application_services.content.clone();
    let workspace_id = fixture.workspace_id;
    let library_id = fixture.library_id;
    let caller = tokio::spawn(async move {
        service
            .append_inline_mutation(
                &state_for_task,
                AppendInlineMutationCommand {
                    base_revision_id,
                    workspace_id,
                    library_id,
                    document_id,
                    idempotency_key: Some(format!("irg199-append-{}", Uuid::now_v7())),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    appended_text: "line three\n".to_string(),
                },
            )
            .await
    });

    let (leased_state, _) = wait_for_attempt_state(
        &pool,
        document_id,
        |state| state == "leased",
        Duration::from_secs(30),
    )
    .await
    .context("the inline append never leased an attempt")?;
    assert_eq!(leased_state, "leased");

    caller.abort();
    let cancelled = caller.await;
    assert!(cancelled.is_err(), "the caller task must be cancelled, not completed");
    Ok(())
}

/// Waits for the append's attempt to leave `leased`. A stranded attempt times
/// out here, which is the RED signal for both cancellation tests.
async fn wait_for_terminal_attempt(
    pool: &PgPool,
    document_id: Uuid,
) -> anyhow::Result<(String, Option<String>)> {
    wait_for_attempt_state(pool, document_id, |state| state != "leased", Duration::from_secs(45))
        .await
        .context("the detached ingest left its attempt leased")
}

async fn attempt_count_for_document(pool: &PgPool, document_id: Uuid) -> anyhow::Result<i64> {
    sqlx::query_scalar(
        "select count(*)
         from ingest_attempt a
         join ingest_job j on j.id = a.job_id
         where j.knowledge_document_id = $1",
    )
    .bind(document_id)
    .fetch_one(pool)
    .await
    .context("failed to count attempts for the document")
}

/// IRG-199: dropping the request future that admitted an inline append must not
/// abort the ingest. The materialization is detached, so the attempt keeps its
/// heartbeat, reaches a terminal state and prepares the revision without the
/// 60s stale-lease reaper requeueing it and redoing the work.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn cancelled_inline_append_finishes_without_a_stale_lease_reap() -> anyhow::Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        seed_clean_v1_ai_catalog(&fixture.state.persistence.postgres)
            .await
            .context("failed to seed the AI catalog for the cancellation test")?;
        configure_ingest_ai_fixture(
            &mut fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            IngestAiFixtureConfig {
                account_label: "IRG-199 cancellation AI fixture",
                account_kind: "AI",
                credential_ref: "test-only-provider-placeholder",
                fixture_name: "irg199-cancellation",
                graph_output: GRAPH_OUTPUT,
                invalid_dimensions_subject: None,
                missing_catalog_subject: "IRG-199 cancellation test",
                // Two seconds per provider call keeps the inline append in
                // flight long enough to drop the caller future at a known
                // point, deterministically.
                io_delay: Some(Duration::from_secs(2)),
                panic_on: None,
            },
        )
        .await
        .context("failed to configure the delayed ingest AI fixture")?;

        let document_id = create_appendable_document(&fixture).await?;
        admit_then_abandon_append(&fixture, document_id).await?;
        let pool = fixture.state.persistence.postgres.clone();

        // Detach: the attempt must leave `leased` on its own well inside the
        // 60s stale-lease window, and never through `stale_heartbeat`. Without
        // the fix the attempt stays `leased` here and this times out.
        let (terminal_state, failure_code) = wait_for_terminal_attempt(&pool, document_id).await?;
        assert_ne!(
            failure_code.as_deref(),
            Some("stale_heartbeat"),
            "the detached ingest must finish on its own, not via the stale-lease reaper",
        );
        assert_eq!(
            terminal_state, "succeeded",
            "the detached inline append must reach a succeeded attempt",
        );
        assert_eq!(
            attempt_count_for_document(&pool, document_id).await?,
            1,
            "the detached ingest must not need a second attempt",
        );

        let appended_revision_id = fixture
            .state
            .application_services
            .content
            .list_revisions(&fixture.state, document_id)
            .await
            .context("failed to list revisions after the cancelled append")?
            .into_iter()
            .filter(|revision| revision.content_source_kind == "append")
            .max_by_key(|revision| revision.revision_number)
            .map(|revision| revision.id)
            .context("the cancelled append did not create a result revision")?;
        let appended_revision = fixture
            .state
            .document_store
            .get_revision(appended_revision_id)
            .await?
            .context("missing appended revision after the cancelled append")?;
        let structured = fixture
            .state
            .document_store
            .get_structured_revision(appended_revision_id)
            .await?
            .context("the cancelled append did not persist a structured artifact")?;
        assert_eq!(
            structured.preparation_state, "prepared",
            "a detached inline append must finalize its structured artifact",
        );
        assert_eq!(appended_revision.text_state, "text_readable");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-199: a panic inside the detached pipeline must not leave the attempt
/// leased. The caller is already gone (detached), so it cannot observe the
/// task's `JoinError`; the in-task unwind guard has to finalize the attempt
/// itself with an explicit failure code.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn panicking_detached_inline_append_is_finalized_not_left_leased() -> anyhow::Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        seed_clean_v1_ai_catalog(&fixture.state.persistence.postgres)
            .await
            .context("failed to seed the AI catalog for the panic test")?;
        configure_ingest_ai_fixture(
            &mut fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            IngestAiFixtureConfig {
                account_label: "IRG-199 panic AI fixture",
                account_kind: "AI",
                credential_ref: "test-only-provider-placeholder",
                fixture_name: "irg199-panic",
                graph_output: GRAPH_OUTPUT,
                invalid_dimensions_subject: None,
                missing_catalog_subject: "IRG-199 panic test",
                // Let embed_chunk finish, then panic in extract_graph two
                // seconds later — after the caller has been dropped.
                io_delay: Some(Duration::from_secs(2)),
                panic_on: Some("generate"),
            },
        )
        .await
        .context("failed to configure the panicking ingest AI fixture")?;

        let document_id = create_appendable_document(&fixture).await?;
        admit_then_abandon_append(&fixture, document_id).await?;
        let pool = fixture.state.persistence.postgres.clone();

        // The panic happens inside the detached task with the caller gone. The
        // attempt must still become terminal, with the explicit panic failure
        // code, and never through `stale_heartbeat`.
        let (terminal_state, failure_code) = wait_for_terminal_attempt(&pool, document_id).await?;
        assert_ne!(
            failure_code.as_deref(),
            Some("stale_heartbeat"),
            "the panic must be finalized in-task, not by the stale-lease reaper",
        );
        assert_eq!(terminal_state, "failed");
        assert_eq!(failure_code.as_deref(), Some("inline_materialization_panicked"));
        assert_eq!(
            attempt_count_for_document(&pool, document_id).await?,
            1,
            "the panic must not require a second attempt",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
