//! PostgreSQL integration coverage for atomic catalog deletion guards.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test catalog_repository -- --include-ignored`

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    domains::webhook::WebhookEvent,
    infra::repositories::{
        catalog_repository::{
            self, CatalogDeleteBlockers, CatalogLibraryDeleteOutcome, CatalogWorkspaceDeleteOutcome,
        },
        webhook_outbox_repository,
    },
};

struct CatalogRepositoryFixture {
    workspace_id: Uuid,
    library_id: Uuid,
}

impl CatalogRepositoryFixture {
    async fn create(pool: &PgPool) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            pool,
            &format!("catalog-delete-{suffix}"),
            "Catalog delete guard workspace",
            None,
        )
        .await
        .context("failed to create catalog delete guard workspace")?;
        let library = catalog_repository::create_library(
            pool,
            workspace.id,
            &format!("catalog-delete-library-{suffix}"),
            "Catalog delete guard library",
            None,
            None,
        )
        .await
        .context("failed to create catalog delete guard library")?;

        Ok(Self { workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(self, pool: &PgPool) -> Result<()> {
        catalog_repository::delete_workspace(pool, self.workspace_id)
            .await
            .context("failed to clean up catalog delete guard workspace")?;
        Ok(())
    }

    async fn create_sibling_library(&self, pool: &PgPool) -> Result<Uuid> {
        let suffix = Uuid::now_v7().simple().to_string();
        let library = catalog_repository::create_library(
            pool,
            self.workspace_id,
            &format!("catalog-delete-sibling-{suffix}"),
            "Catalog delete guard sibling library",
            None,
            None,
        )
        .await
        .context("failed to create sibling catalog delete guard library")?;
        Ok(library.id)
    }
}

async fn connect_postgres() -> Result<PgPool> {
    let settings = Settings::from_env().context("failed to load catalog repository settings")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect catalog repository postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply catalog repository migrations")?;
    Ok(pool)
}

async fn assert_library_exists(pool: &PgPool, library_id: Uuid) -> Result<()> {
    let exists = catalog_repository::get_library_by_id(pool, library_id)
        .await
        .context("failed to reload guarded library")?
        .is_some();
    assert!(exists, "a blocked deletion must preserve the library row");
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn query_visible_library_config_advances_answer_cache_generation() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let before =
        catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
    let library = catalog_repository::get_library_by_id(&pool, fixture.library_id)
        .await?
        .context("fixture library missing")?;

    catalog_repository::update_library_retrieval_config(
        &pool,
        fixture.library_id,
        library.retrieval_config,
    )
    .await?
    .context("retrieval config update did not find fixture library")?;
    let after =
        catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
    assert!(
        after > before,
        "query-visible library config must invalidate every replica's answer cache identity",
    );

    fixture.cleanup(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn library_delete_waits_for_lifecycle_outbox_terminal_resolution() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let outbox_id = Uuid::now_v7();

    sqlx::query(
        "insert into webhook_lifecycle_outbox (
            id, event_id, event_type, occurred_at, workspace_id, library_id, payload_json
         ) values ($1, $2, 'document.deleted', now(), $3, $4, $5)",
    )
    .bind(outbox_id)
    .bind(format!("document.deleted:catalog-delete-guard:{outbox_id}"))
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(serde_json::json!({ "documentId": Uuid::now_v7() }))
    .execute(&pool)
    .await
    .context("failed to insert pending lifecycle outbox event")?;

    let pending = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("pending outbox guard query failed")?;
    assert_eq!(
        pending,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        })
    );
    assert_library_exists(&pool, fixture.library_id).await?;

    sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dead_letter',
             last_error_code = 'fanout_failed',
             last_error = 'redacted failure',
             updated_at = now()
         where id = $1",
    )
    .bind(outbox_id)
    .execute(&pool)
    .await
    .context("failed to dead-letter lifecycle outbox event")?;

    let dead_letter = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("dead-letter outbox guard query failed")?;
    assert_eq!(
        dead_letter,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        })
    );
    assert_library_exists(&pool, fixture.library_id).await?;

    let resolved = webhook_outbox_repository::resolve_dead_letter_webhook_lifecycle_outbox(
        &pool,
        outbox_id,
        "receiver_retired",
    )
    .await
    .context("failed to resolve lifecycle outbox dead-letter")?
    .context("dead-letter resolution lost its compare-and-set")?;
    assert_eq!(resolved.dispatch_state, "resolved");
    assert!(resolved.dispatched_at.is_none(), "resolution must not claim delivery");

    let deleted = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("drained outbox delete failed")?;
    assert_eq!(deleted, CatalogLibraryDeleteOutcome::Deleted);
    let durable_resolution_audit = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from audit_event_subject subject
         join audit_event event on event.id = subject.audit_event_id
         where subject.subject_kind = 'webhook_lifecycle_outbox'
           and subject.subject_id = $1
           and event.action_kind = 'webhook.lifecycle_outbox.dead_letter_resolved'",
    )
    .bind(outbox_id)
    .fetch_one(&pool)
    .await
    .context("failed to verify durable outbox resolution audit")?;
    assert_eq!(durable_resolution_audit, 1);
    assert!(
        catalog_repository::get_library_by_id(&pool, fixture.library_id)
            .await
            .context("failed to verify deleted library")?
            .is_none()
    );
    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id)
            .await
            .context("missing library delete outcome failed")?,
        CatalogLibraryDeleteOutcome::NotFound
    );

    sqlx::query(
        "delete from audit_event
         where id in (
             select audit_event_id
             from audit_event_subject
             where subject_kind = 'webhook_lifecycle_outbox'
               and subject_id = $1
         )",
    )
    .bind(outbox_id)
    .execute(&pool)
    .await
    .context("failed to clean up outbox resolution audit")?;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn library_delete_waits_for_active_webhook_delivery_job_to_finish() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let job_id = Uuid::now_v7();

    sqlx::query(
        "insert into ingest_job (
            id, workspace_id, library_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at
         ) values (
            $1, $2, $3, 'webhook_delivery', 'queued', 100,
            $4, now(), now()
         )",
    )
    .bind(job_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("catalog-delete-guard:{job_id}"))
    .execute(&pool)
    .await
    .context("failed to insert webhook delivery job")?;

    for queue_state in ["queued", "leased", "paused", "failed"] {
        sqlx::query(
            "update ingest_job
             set queue_state = $2::ingest_queue_state,
                 completed_at = case when $2 = 'failed' then now() else null end,
                 queue_leased_at = case when $2 = 'leased' then now() else null end,
                 queue_lease_token = case when $2 = 'leased' then 'catalog-delete-test' else null end,
                 queue_lease_owner = case when $2 = 'leased' then 'catalog-delete-test' else null end
             where id = $1",
        )
        .bind(job_id)
        .bind(queue_state)
        .execute(&pool)
        .await
        .with_context(|| format!("failed to set webhook delivery job state {queue_state}"))?;

        let blocked = catalog_repository::delete_library(&pool, fixture.library_id)
            .await
            .with_context(|| format!("{queue_state} webhook job guard query failed"))?;
        assert_eq!(
            blocked,
            CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
                undispatched_webhook_lifecycle_events: 0,
                active_webhook_delivery_jobs: 1,
                unresolved_webhook_delivery_attempts: 0,
            }),
            "queue state {queue_state} must block library deletion"
        );
        assert_library_exists(&pool, fixture.library_id).await?;
    }

    sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1",
    )
    .bind(job_id)
    .execute(&pool)
    .await
    .context("failed to complete webhook delivery job")?;

    let deleted = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("drained webhook delivery job delete failed")?;
    assert_eq!(deleted, CatalogLibraryDeleteOutcome::Deleted);

    fixture.cleanup(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn library_delete_waits_for_an_unlinked_pending_delivery_attempt() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let subscription_id = Uuid::now_v7();
    let attempt_id = Uuid::now_v7();

    sqlx::query(
        "insert into webhook_subscription (
            id, workspace_id, library_id, display_name, target_url, secret,
            event_types, custom_headers_json
         ) values (
            $1, $2, $3, 'Delete guard subscription', 'https://example.com/webhook',
            'legacy-synthetic-signing-secret', array['revision.ready']::text[], '{}'::jsonb
         )",
    )
    .bind(subscription_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .execute(&pool)
    .await
    .context("failed to insert delete-guard subscription")?;
    sqlx::query(
        "insert into webhook_delivery_attempt (
            id, subscription_id, workspace_id, library_id, event_type, event_id,
            occurred_at, payload_json, target_url
         ) values (
            $1, $2, $3, $4, 'revision.ready', $5, now(), $6,
            'https://example.com/webhook'
         )",
    )
    .bind(attempt_id)
    .bind(subscription_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("revision.ready:delete-guard:{attempt_id}"))
    .bind(serde_json::json!({ "revision_id": Uuid::now_v7() }))
    .execute(&pool)
    .await
    .context("failed to insert unlinked delivery attempt")?;

    let blocked = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("unlinked delivery-attempt guard query failed")?;
    assert_eq!(
        blocked,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 1,
        })
    );
    assert_library_exists(&pool, fixture.library_id).await?;

    sqlx::query(
        "update webhook_delivery_attempt
         set delivery_state = 'delivered', delivered_at = now(), updated_at = now()
         where id = $1",
    )
    .bind(attempt_id)
    .execute(&pool)
    .await
    .context("failed to terminalize unlinked delivery attempt")?;

    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id)
            .await
            .context("terminal delivery-attempt delete failed")?,
        CatalogLibraryDeleteOutcome::Deleted
    );

    fixture.cleanup(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn workspace_delete_waits_for_lifecycle_outbox_to_be_dispatched() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let blocked_library_id = fixture.create_sibling_library(&pool).await?;
    let outbox_id = Uuid::now_v7();

    sqlx::query(
        "insert into webhook_lifecycle_outbox (
            id, event_id, event_type, occurred_at, workspace_id, library_id, payload_json
         ) values ($1, $2, 'document.deleted', now(), $3, $4, $5)",
    )
    .bind(outbox_id)
    .bind(format!("document.deleted:workspace-delete-guard:{outbox_id}"))
    .bind(fixture.workspace_id)
    .bind(blocked_library_id)
    .bind(serde_json::json!({ "documentId": Uuid::now_v7() }))
    .execute(&pool)
    .await
    .context("failed to insert workspace-scoped pending lifecycle outbox event")?;

    let pending = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
        .await
        .context("pending workspace outbox guard query failed")?;
    assert_eq!(
        pending,
        CatalogWorkspaceDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        })
    );
    assert_library_exists(&pool, fixture.library_id).await?;
    assert_library_exists(&pool, blocked_library_id).await?;

    sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dead_letter',
             last_error_code = 'fanout_failed',
             last_error = 'redacted failure',
             updated_at = now()
         where id = $1",
    )
    .bind(outbox_id)
    .execute(&pool)
    .await
    .context("failed to dead-letter workspace lifecycle outbox event")?;

    let dead_letter = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
        .await
        .context("dead-letter workspace outbox guard query failed")?;
    assert_eq!(
        dead_letter,
        CatalogWorkspaceDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        })
    );
    assert_library_exists(&pool, fixture.library_id).await?;
    assert_library_exists(&pool, blocked_library_id).await?;

    sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dispatched',
             dispatched_at = now(),
             last_error_code = null,
             last_error = null,
             updated_at = now()
         where id = $1",
    )
    .bind(outbox_id)
    .execute(&pool)
    .await
    .context("failed to mark workspace lifecycle outbox event dispatched")?;

    let deleted = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
        .await
        .context("drained workspace outbox delete failed")?;
    assert_eq!(deleted, CatalogWorkspaceDeleteOutcome::Deleted);
    assert_eq!(
        catalog_repository::delete_workspace(&pool, fixture.workspace_id)
            .await
            .context("missing workspace delete outcome failed")?,
        CatalogWorkspaceDeleteOutcome::NotFound
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn workspace_delete_waits_for_active_webhook_delivery_job_to_finish() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let blocked_library_id = fixture.create_sibling_library(&pool).await?;
    let job_id = Uuid::now_v7();

    sqlx::query(
        "insert into ingest_job (
            id, workspace_id, library_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at
         ) values (
            $1, $2, $3, 'webhook_delivery', 'queued', 100,
            $4, now(), now()
         )",
    )
    .bind(job_id)
    .bind(fixture.workspace_id)
    .bind(blocked_library_id)
    .bind(format!("workspace-delete-guard:{job_id}"))
    .execute(&pool)
    .await
    .context("failed to insert workspace webhook delivery job")?;

    for queue_state in ["queued", "leased", "paused", "failed"] {
        sqlx::query(
            "update ingest_job
             set queue_state = $2::ingest_queue_state,
                 completed_at = case when $2 = 'failed' then now() else null end,
                 queue_leased_at = case when $2 = 'leased' then now() else null end,
                 queue_lease_token = case when $2 = 'leased' then 'workspace-delete-test' else null end,
                 queue_lease_owner = case when $2 = 'leased' then 'workspace-delete-test' else null end
             where id = $1",
        )
        .bind(job_id)
        .bind(queue_state)
        .execute(&pool)
        .await
        .with_context(|| format!("failed to set workspace webhook job state {queue_state}"))?;

        let blocked = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
            .await
            .with_context(|| format!("{queue_state} workspace webhook job guard query failed"))?;
        assert_eq!(
            blocked,
            CatalogWorkspaceDeleteOutcome::Blocked(CatalogDeleteBlockers {
                undispatched_webhook_lifecycle_events: 0,
                active_webhook_delivery_jobs: 1,
                unresolved_webhook_delivery_attempts: 0,
            }),
            "queue state {queue_state} must block workspace deletion"
        );
        assert_library_exists(&pool, fixture.library_id).await?;
        assert_library_exists(&pool, blocked_library_id).await?;
    }

    sqlx::query(
        "update ingest_job
         set queue_state = 'canceled',
             completed_at = now(),
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1",
    )
    .bind(job_id)
    .execute(&pool)
    .await
    .context("failed to cancel workspace webhook delivery job")?;

    let deleted = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
        .await
        .context("drained workspace webhook job delete failed")?;
    assert_eq!(deleted, CatalogWorkspaceDeleteOutcome::Deleted);

    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn lifecycle_producer_parent_lock_prevents_catalog_delete_deadlock() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let document_id = Uuid::now_v7();
    let job_id = Uuid::now_v7();
    sqlx::query(
        "insert into content_document (
            id, workspace_id, library_id, external_key, document_state
         ) values ($1, $2, $3, $4, 'active')",
    )
    .bind(document_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("catalog-delete-lock-order:{document_id}"))
    .execute(&pool)
    .await?;
    sqlx::query(
        "insert into ingest_job (
            id, workspace_id, library_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at
         ) values (
            $1, $2, $3, 'webhook_delivery', 'queued', 100,
            $4, now(), now()
         )",
    )
    .bind(job_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("catalog-delete-lock-order:{job_id}"))
    .execute(&pool)
    .await?;

    let event = WebhookEvent {
        event_type: "document.deleted".to_string(),
        event_id: format!("document.deleted:{document_id}:lock-order"),
        occurred_at: chrono::Utc::now(),
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        payload_json: serde_json::json!({ "document_id": document_id }),
    };
    let mut producer = pool.begin().await?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *producer,
        fixture.workspace_id,
        fixture.library_id,
    )
    .await?;
    assert!(parent_locked, "producer must lock the live parent library first");

    let delete_pool = pool.clone();
    let library_id = fixture.library_id;
    let delete_task =
        tokio::spawn(
            async move { catalog_repository::delete_library(&delete_pool, library_id).await },
        );
    tokio::task::yield_now().await;

    sqlx::query("select id from content_document where id = $1 for update")
        .bind(document_id)
        .execute(&mut *producer)
        .await?;
    sqlx::query("select id from ingest_job where id = $1 for update")
        .bind(job_id)
        .execute(&mut *producer)
        .await?;
    webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(
        &mut *producer,
        &event,
    )
    .await?;
    producer.commit().await?;

    let delete_join_result = tokio::time::timeout(Duration::from_secs(5), delete_task)
        .await
        .context("catalog delete deadlocked behind lifecycle producer")?;
    let delete_result = delete_join_result.context("catalog delete task panicked")?;
    let delete_outcome = delete_result.context("catalog delete query failed")?;
    assert_eq!(
        delete_outcome,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 1,
            unresolved_webhook_delivery_attempts: 0,
        })
    );

    sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dispatched', dispatched_at = now(), updated_at = now()
         where event_id = $1",
    )
    .bind(&event.event_id)
    .execute(&pool)
    .await?;
    sqlx::query(
        "update ingest_job
         set queue_state = 'completed', completed_at = now(),
             queue_leased_at = null, queue_lease_token = null, queue_lease_owner = null
         where id = $1",
    )
    .bind(job_id)
    .execute(&pool)
    .await?;
    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Deleted
    );

    fixture.cleanup(&pool).await?;
    pool.close().await;
    Ok(())
}
