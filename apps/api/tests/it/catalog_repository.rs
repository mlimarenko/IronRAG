//! PostgreSQL integration coverage for atomic catalog deletion guards.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --include-ignored catalog_repository::`

use crate::temp_postgres_database;

use std::{ops::Deref, time::Duration};

use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    domains::webhook::WebhookEvent,
    infra::repositories::{
        catalog_repository::{
            self, AdminLibraryFacetColumn, AdminLibraryFacetCursorValue, AdminLibraryFacetQuery,
            AdminLibraryFilterValues, AdminLibraryListFilters, AdminLibraryPageOrder,
            AdminLibraryPageQuery, AdminLibrarySearchScope, CatalogDeleteBlockers,
            CatalogLibraryDeleteOutcome, CatalogWorkspaceDeleteOutcome,
        },
        ingest_repository, webhook_outbox_repository,
    },
};
use temp_postgres_database::TempPostgresDatabase;

struct CatalogRepositoryFixture {
    workspace_id: Uuid,
    library_id: Uuid,
}

#[derive(Clone, Copy)]
struct ContentMutationGraph {
    document_id: Uuid,
    mutation_id: Uuid,
    mutation_item_id: Uuid,
}

impl ContentMutationGraph {
    async fn create(
        pool: &PgPool,
        workspace_id: Uuid,
        library_id: Uuid,
        label: &str,
    ) -> Result<Self> {
        let document_id = Uuid::now_v7();
        let mutation_item_id = Uuid::now_v7();

        sqlx::query(
            "insert into content_document (
                id, workspace_id, library_id, external_key, document_state
             ) values ($1, $2, $3, $4, 'active')",
        )
        .bind(document_id)
        .bind(workspace_id)
        .bind(library_id)
        .bind(format!("catalog-delete-{label}:{document_id}"))
        .execute(pool)
        .await
        .context("failed to create catalog deletion document")?;
        let identity = serde_json::json!({
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "operationKind": "upload",
            "target": {"kind": "existing", "documentId": document_id},
            "sourceIdentity": null,
        });
        let mutation_id = crate::content_lifecycle_support::seed_pending_content_mutation(
            pool,
            crate::content_lifecycle_support::PendingContentMutationSeed {
                workspace_id,
                library_id,
                operation_kind: "upload",
                request_surface: "internal",
                mutation_state: "accepted",
                request_identity: &identity,
            },
        )
        .await
        .context("failed to create catalog deletion mutation")?
        .id;
        sqlx::query(
            "insert into content_mutation_item (id, mutation_id, document_id)
             values ($1, $2, $3)",
        )
        .bind(mutation_item_id)
        .bind(mutation_id)
        .bind(document_id)
        .execute(pool)
        .await
        .context("failed to create catalog deletion mutation item")?;

        Ok(Self { document_id, mutation_id, mutation_item_id })
    }

    async fn row_counts(self, pool: &PgPool) -> Result<(i64, i64, i64)> {
        sqlx::query_as::<_, (i64, i64, i64)>(
            "select
                (select count(*)::bigint from content_document where id = $1),
                (select count(*)::bigint from content_mutation where id = $2),
                (select count(*)::bigint from content_mutation_item where id = $3)",
        )
        .bind(self.document_id)
        .bind(self.mutation_id)
        .bind(self.mutation_item_id)
        .fetch_one(pool)
        .await
        .context("failed to inspect catalog deletion mutation graph")
    }

    async fn assert_exists(self, pool: &PgPool) -> Result<()> {
        assert_eq!(self.row_counts(pool).await?, (1, 1, 1));
        Ok(())
    }

    async fn assert_deleted(self, pool: &PgPool) -> Result<()> {
        assert_eq!(self.row_counts(pool).await?, (0, 0, 0));
        Ok(())
    }
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

struct CatalogRepositoryDatabase {
    pool: PgPool,
    database: TempPostgresDatabase,
}

impl Deref for CatalogRepositoryDatabase {
    type Target = PgPool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl CatalogRepositoryDatabase {
    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

async fn connect_postgres() -> Result<CatalogRepositoryDatabase> {
    let database_url =
        std::env::var("IRONRAG_DATABASE_URL").context("IRONRAG_DATABASE_URL must be configured")?;
    let database = TempPostgresDatabase::create(&database_url, "catalog_repository").await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database.database_url)
        .await
        .context("failed to connect catalog repository postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply catalog repository migrations")?;
    Ok(CatalogRepositoryDatabase { pool, database })
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
async fn library_delete_removes_target_mutation_graph_and_preserves_sibling() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let sibling_library_id = fixture.create_sibling_library(&pool).await?;
    let target_graph = ContentMutationGraph::create(
        &pool,
        fixture.workspace_id,
        fixture.library_id,
        "target-library",
    )
    .await?;
    let sibling_graph = ContentMutationGraph::create(
        &pool,
        fixture.workspace_id,
        sibling_library_id,
        "sibling-library",
    )
    .await?;

    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Deleted
    );
    target_graph.assert_deleted(&pool).await?;
    sibling_graph.assert_exists(&pool).await?;
    assert_library_exists(&pool, sibling_library_id).await?;

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn workspace_delete_removes_every_target_mutation_graph_and_preserves_sibling_workspace()
-> Result<()> {
    let pool = connect_postgres().await?;
    let target = CatalogRepositoryFixture::create(&pool).await?;
    let target_sibling_library_id = target.create_sibling_library(&pool).await?;
    let sibling_workspace = CatalogRepositoryFixture::create(&pool).await?;
    let target_graph = ContentMutationGraph::create(
        &pool,
        target.workspace_id,
        target.library_id,
        "target-workspace-primary",
    )
    .await?;
    let target_sibling_graph = ContentMutationGraph::create(
        &pool,
        target.workspace_id,
        target_sibling_library_id,
        "target-workspace-sibling",
    )
    .await?;
    let surviving_graph = ContentMutationGraph::create(
        &pool,
        sibling_workspace.workspace_id,
        sibling_workspace.library_id,
        "sibling-workspace",
    )
    .await?;

    assert_eq!(
        catalog_repository::delete_workspace(&pool, target.workspace_id).await?,
        CatalogWorkspaceDeleteOutcome::Deleted
    );
    target_graph.assert_deleted(&pool).await?;
    target_sibling_graph.assert_deleted(&pool).await?;
    surviving_graph.assert_exists(&pool).await?;
    assert_library_exists(&pool, sibling_workspace.library_id).await?;

    sibling_workspace.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn blocked_library_delete_preserves_mutation_graph() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let graph = ContentMutationGraph::create(
        &pool,
        fixture.workspace_id,
        fixture.library_id,
        "blocked-library",
    )
    .await?;
    let outbox_id = Uuid::now_v7();
    sqlx::query(
        "insert into webhook_lifecycle_outbox (
            id, event_id, event_type, occurred_at, workspace_id, library_id, payload_json
         ) values ($1, $2, 'document.deleted', now(), $3, $4, $5)",
    )
    .bind(outbox_id)
    .bind(format!("document.deleted:catalog-delete-mutation:{outbox_id}"))
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(serde_json::json!({ "documentId": graph.document_id }))
    .execute(&*pool)
    .await?;

    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        })
    );
    graph.assert_exists(&pool).await?;
    assert_library_exists(&pool, fixture.library_id).await?;

    sqlx::query(
        "update webhook_lifecycle_outbox
         set dispatch_state = 'dispatched', dispatched_at = now(), updated_at = now()
         where id = $1",
    )
    .bind(outbox_id)
    .execute(&*pool)
    .await?;
    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
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
    pool.cleanup().await?;
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
    .execute(&*pool)
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
    .execute(&*pool)
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
        &*pool,
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
    .fetch_one(&*pool)
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
    .execute(&*pool)
    .await
    .context("failed to clean up outbox resolution audit")?;

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
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
    .execute(&*pool)
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
        .execute(&*pool)
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
    .execute(&*pool)
    .await
    .context("failed to complete webhook delivery job")?;

    let deleted = catalog_repository::delete_library(&pool, fixture.library_id)
        .await
        .context("drained webhook delivery job delete failed")?;
    assert_eq!(deleted, CatalogLibraryDeleteOutcome::Deleted);

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn library_delete_quiesces_content_jobs_while_webhook_work_drains() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let graph_job_id = Uuid::now_v7();
    let webhook_job_id = Uuid::now_v7();

    sqlx::query(
        "insert into ingest_job (
            id, workspace_id, library_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at
         ) values
            ($1, $3, $4, 'graph_refresh', 'queued', 200, $5, now(), now()),
            ($2, $3, $4, 'webhook_delivery', 'queued', 100, $6, now(), now())",
    )
    .bind(graph_job_id)
    .bind(webhook_job_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(format!("graph-repair:{graph_job_id}"))
    .bind(format!("webhook-delivery:{webhook_job_id}"))
    .execute(&*pool)
    .await
    .context("failed to insert deletion quiescence jobs")?;

    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Blocked(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 1,
            unresolved_webhook_delivery_attempts: 0,
        })
    );

    let state =
        sqlx::query_as::<_, (String, String, Option<chrono::DateTime<chrono::Utc>>, String)>(
            "select
            library.lifecycle_state::text,
            graph_job.queue_state::text,
            graph_job.completed_at,
            webhook_job.queue_state::text
         from catalog_library library
         join ingest_job graph_job on graph_job.id = $2
         join ingest_job webhook_job on webhook_job.id = $3
         where library.id = $1",
        )
        .bind(fixture.library_id)
        .bind(graph_job_id)
        .bind(webhook_job_id)
        .fetch_one(&*pool)
        .await
        .context("failed to inspect quiesced library deletion")?;
    assert_eq!(state.0, "archived");
    assert_eq!(state.1, "canceled");
    assert!(state.2.is_some());
    assert_eq!(state.3, "queued");

    sqlx::query(
        "update ingest_job
         set queue_state = 'completed', completed_at = now()
         where id = $1",
    )
    .bind(webhook_job_id)
    .execute(&*pool)
    .await
    .context("failed to complete deletion-blocking webhook job")?;
    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Deleted
    );

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
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
    .execute(&*pool)
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
    .execute(&*pool)
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
    .execute(&*pool)
    .await
    .context("failed to terminalize unlinked delivery attempt")?;

    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id)
            .await
            .context("terminal delivery-attempt delete failed")?,
        CatalogLibraryDeleteOutcome::Deleted
    );

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
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
    .execute(&*pool)
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
    .execute(&*pool)
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
    .execute(&*pool)
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

    pool.cleanup().await?;
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
    .execute(&*pool)
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
        .execute(&*pool)
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
    .execute(&*pool)
    .await
    .context("failed to cancel workspace webhook delivery job")?;

    let deleted = catalog_repository::delete_workspace(&pool, fixture.workspace_id)
        .await
        .context("drained workspace webhook job delete failed")?;
    assert_eq!(deleted, CatalogWorkspaceDeleteOutcome::Deleted);

    pool.cleanup().await?;
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
    .execute(&*pool)
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
    .execute(&*pool)
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
    .execute(&*pool)
    .await?;
    sqlx::query(
        "update ingest_job
         set queue_state = 'completed', completed_at = now(),
             queue_leased_at = null, queue_lease_token = null, queue_lease_owner = null
         where id = $1",
    )
    .bind(job_id)
    .execute(&*pool)
    .await?;
    assert_eq!(
        catalog_repository::delete_library(&pool, fixture.library_id).await?,
        CatalogLibraryDeleteOutcome::Deleted
    );

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admin_library_page_counts_only_queued_jobs_per_library() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let sibling_library_id = fixture.create_sibling_library(&pool).await?;

    // Target library: two queued jobs survive alongside leased, paused and
    // terminal rows, so only `queue_state = 'queued'` may be counted.
    for (label, queue_state) in [
        ("target-queued-a", "queued"),
        ("target-queued-b", "queued"),
        ("target-leased", "leased"),
        ("target-paused", "paused"),
        ("target-failed", "failed"),
        ("target-completed", "completed"),
    ] {
        insert_admin_queue_job(&pool, fixture.workspace_id, fixture.library_id, label, queue_state)
            .await?;
    }

    // Sibling library: no queued job, but active and terminal rows exist, so the
    // zero count must come from the predicate rather than an empty table.
    for (label, queue_state) in [
        ("sibling-leased", "leased"),
        ("sibling-paused", "paused"),
        ("sibling-completed", "completed"),
    ] {
        insert_admin_queue_job(&pool, fixture.workspace_id, sibling_library_id, label, queue_state)
            .await?;
    }

    let required_binding_purposes: Vec<String> = Vec::new();
    let filters = AdminLibraryListFilters::default();
    let (rows, _facets) = catalog_repository::list_admin_library_page(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &filters,
            required_binding_purposes: &required_binding_purposes,
            offset: 0,
            limit: 10,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
        },
    )
    .await?;

    let queued_by_library: std::collections::HashMap<Uuid, i64> =
        rows.iter().map(|row| (row.id, row.queued_job_count)).collect();
    assert_eq!(queued_by_library.get(&fixture.library_id).copied(), Some(2));
    assert_eq!(queued_by_library.get(&sibling_library_id).copied(), Some(0));

    // The administration row and the operator queue summary must agree: both
    // count the same `ingest_job` rows whose `queue_state` is `queued`.
    for (library_id, expected) in [(fixture.library_id, 2_i64), (sibling_library_id, 0_i64)] {
        let queue_rows =
            ingest_repository::list_active_ingest_queue(&pool, None, Some(library_id)).await?;
        let queued = queue_rows.iter().filter(|row| row.queue_state == "queued").count() as i64;
        assert_eq!(
            queued, expected,
            "list_ingest_queue queued summary must match the admin page count for library {library_id}"
        );
    }

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

async fn insert_admin_queue_job(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    label: &str,
    queue_state: &str,
) -> Result<()> {
    let job_id = Uuid::now_v7();
    sqlx::query(
        "insert into ingest_job (
            id, workspace_id, library_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at, completed_at
         ) values (
            $1, $2, $3, 'graph_refresh', $4::ingest_queue_state, 100,
            $5, now(), now(),
            case when $4 in ('failed', 'completed') then now() else null end
         )",
    )
    .bind(job_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(queue_state)
    .bind(format!("admin-library-queue:{library_id}:{label}:{job_id}"))
    .execute(pool)
    .await
    .with_context(|| format!("failed to insert {queue_state} admin queue job {label}"))?;
    Ok(())
}

/// Leave a library's billing rollup open (`applied < dirty`) so the
/// administration grid displays NULL for its documents/calls/cost columns.
async fn insert_open_cost_rollup(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into billing_execution_cost_rollup_state (
            owning_execution_kind, owning_execution_id, workspace_id, library_id,
            dirty_generation, applied_generation, dirty_at, repair_attempts,
            next_attempt_at, last_error
         ) values (
            'query_execution'::billing_owning_execution_kind, $1, $2, $3,
            1, 0, now(), 0, now(), null
         )",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(library_id)
    .execute(pool)
    .await
    .context("failed to open the admin library cost rollup")?;
    Ok(())
}

async fn admin_library_rows(
    pool: &PgPool,
    query: AdminLibraryPageQuery<'_>,
) -> Result<Vec<catalog_repository::AdminLibraryPageRow>> {
    let (rows, _facets) = catalog_repository::list_admin_library_page(pool, query).await?;
    Ok(rows)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admin_library_grid_search_filters_order_and_self_excluding_facets() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let sibling = fixture.create_sibling_library(&pool).await?;

    // Main library: two queued jobs and an open rollup (NULL documents/calls/cost).
    // Sibling: no queued job and a readable rollup (0 documents).
    insert_admin_queue_job(&pool, fixture.workspace_id, fixture.library_id, "a", "queued").await?;
    insert_admin_queue_job(&pool, fixture.workspace_id, fixture.library_id, "b", "queued").await?;
    insert_admin_queue_job(&pool, fixture.workspace_id, sibling, "leased", "leased").await?;
    insert_open_cost_rollup(&pool, fixture.workspace_id, fixture.library_id).await?;

    let purposes: Vec<String> = Vec::new();
    let none = AdminLibraryListFilters::default();

    // Queue ascending: sibling (0) before main (2); offset pages stay stable.
    let page1 = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Queue, desc: false },
            offset: 0,
            limit: 1,
        },
    )
    .await?;
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].id, sibling);
    let page2 = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Queue, desc: false },
            offset: 1,
            limit: 1,
        },
    )
    .await?;
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, fixture.library_id);
    assert_eq!(page2[0].queued_job_count, 2);

    // The whole-table search reaches a numeric displayed value: numbers=[2] finds
    // the library whose queue count is 2, not the one with 0.
    let numeric = AdminLibrarySearchScope { numbers: &[2], ..AdminLibrarySearchScope::empty() };
    let numeric_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: numeric,
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(numeric_rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![fixture.library_id]);

    // A documents search must not match a healthy rollup's real zero.
    let zero_documents =
        AdminLibrarySearchScope { numbers: &[0], ..AdminLibrarySearchScope::empty() };
    let zero_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: zero_documents,
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(zero_rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![sibling]);

    // The page's own unavailable/unknown label selects the blocked rows whose
    // cost/documents/calls columns really render NULL.
    let unavailable =
        AdminLibrarySearchScope { unavailable: true, ..AdminLibrarySearchScope::empty() };
    let unavailable_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: unavailable,
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(
        unavailable_rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![fixture.library_id]
    );

    // OR within one column, AND across columns.
    let or_filter = AdminLibraryListFilters {
        queue: AdminLibraryFilterValues { present: true, include_null: false, values: vec![0, 2] },
        ..AdminLibraryListFilters::default()
    };
    let or_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &or_filter,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(or_rows.len(), 2);

    let and_filter = AdminLibraryListFilters {
        queue: AdminLibraryFilterValues { present: true, include_null: false, values: vec![2] },
        lifecycle: AdminLibraryFilterValues {
            present: true,
            include_null: false,
            values: vec!["active".to_string()],
        },
        ..AdminLibraryListFilters::default()
    };
    let and_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &and_filter,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(and_rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![fixture.library_id]);

    // NULL documents sort last on both directions; the healthy zero sorts first.
    for desc in [false, true] {
        let documents_order = admin_library_rows(
            &pool,
            AdminLibraryPageQuery {
                scope: AdminLibrarySearchScope::empty(),
                filters: &none,
                required_binding_purposes: &purposes,
                order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Documents, desc },
                offset: 0,
                limit: 10,
            },
        )
        .await?;
        assert_eq!(
            documents_order.first().map(|row| row.id),
            Some(sibling),
            "healthy zero must sort before the NULL bucket (desc={desc})"
        );
        assert!(
            documents_order
                .iter()
                .find(|row| row.id == fixture.library_id)
                .is_some_and(|row| { row.cost_blocked })
        );
    }

    // Facet self-exclusion: filtering queue=[2] must not hide queue value 0 from
    // the queue facet, even though it does anchor the list to one library.
    let queue_two = AdminLibraryListFilters {
        queue: AdminLibraryFilterValues { present: true, include_null: false, values: vec![2] },
        ..AdminLibraryListFilters::default()
    };
    let facet_first = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &queue_two,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Queue,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 1,
        },
    )
    .await?;
    assert_eq!(facet_first.rows.len(), 1);
    assert!(facet_first.has_more);
    assert_eq!(facet_first.rows[0].value_number, Some(0));
    let cursor = AdminLibraryFacetCursorValue::Number(0);
    let facet_second = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &queue_two,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Queue,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: Some(&cursor),
            limit: 1,
        },
    )
    .await?;
    assert_eq!(facet_second.rows.len(), 1);
    assert_eq!(facet_second.rows[0].value_number, Some(2));
    assert!(!facet_second.has_more);

    // The documents facet keeps the NULL bucket for the open rollup.
    let documents_facet = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Documents,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 10,
        },
    )
    .await?;
    assert!(documents_facet.rows.iter().any(|row| {
        row.value_number.is_none() && row.value_decimal.is_none() && row.value_text.is_none()
    }));

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

struct LibraryExecutionCost<'a> {
    total_cost: &'a str,
    currency_code: &'a str,
    provider_call_count: i32,
    unpriced_provider_call_count: i32,
}

async fn insert_execution_cost(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    cost: LibraryExecutionCost<'_>,
) -> Result<()> {
    sqlx::query(
        "insert into billing_execution_cost (
            owning_execution_kind, owning_execution_id, total_cost, currency_code,
            provider_call_count, unpriced_provider_call_count, workspace_id, library_id
         ) values (
            'query_execution'::billing_owning_execution_kind, $1, $2::numeric, $3, $4, $5, $6, $7
         )",
    )
    .bind(Uuid::now_v7())
    .bind(cost.total_cost)
    .bind(cost.currency_code)
    .bind(cost.provider_call_count)
    .bind(cost.unpriced_provider_call_count)
    .bind(workspace_id)
    .bind(library_id)
    .execute(pool)
    .await
    .context("failed to insert execution cost")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admin_library_cost_filter_preserves_selected_tuples() -> Result<()> {
    use ironrag_contracts::admin::{AdminLibraryCostCoverage, AdminLibraryCostValue};
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let eur = fixture.create_sibling_library(&pool).await?;
    let cross_product = fixture.create_sibling_library(&pool).await?;
    let different_coverage = fixture.create_sibling_library(&pool).await?;
    let different_unpriced = fixture.create_sibling_library(&pool).await?;
    for (id, amount, currency, unpriced) in [
        (fixture.library_id, "1.2345", "USD", 1),
        (eur, "2.34", "EUR", 0),
        (cross_product, "2.34", "USD", 1),
        (different_coverage, "1.23", "USD", 0),
        (different_unpriced, "1.23", "USD", 2),
    ] {
        insert_execution_cost(
            &pool,
            fixture.workspace_id,
            id,
            LibraryExecutionCost {
                total_cost: amount,
                currency_code: currency,
                provider_call_count: 4,
                unpriced_provider_call_count: unpriced,
            },
        )
        .await?;
    }
    let filters = AdminLibraryListFilters {
        cost: AdminLibraryFilterValues {
            present: true,
            include_null: false,
            values: vec![
                AdminLibraryCostValue {
                    amount: "1.23".into(),
                    currency_code: "USD".into(),
                    coverage: AdminLibraryCostCoverage::Partial,
                    unpriced_call_count: 1,
                },
                AdminLibraryCostValue {
                    amount: "2.34".into(),
                    currency_code: "EUR".into(),
                    coverage: AdminLibraryCostCoverage::Complete,
                    unpriced_call_count: 0,
                },
            ],
        },
        ..AdminLibraryListFilters::default()
    };
    let rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &filters,
            required_binding_purposes: &[],
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    let ids: std::collections::HashSet<_> = rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        [fixture.library_id, eur].into_iter().collect(),
        "OR selects complete cost tuples, not a Cartesian product or other displayed decorations"
    );
    let terms: Vec<_> = filters
        .cost
        .values
        .iter()
        .map(|value| ironrag_contracts::admin::AdminLibraryCostSearch {
            amount: Some(value.amount.clone()),
            currency_code: Some(value.currency_code.clone()),
            coverage: Some(value.coverage),
            unpriced_call_count: Some(value.unpriced_call_count),
        })
        .collect();
    let no_filters = AdminLibraryListFilters::default();
    let scope = AdminLibrarySearchScope {
        search: Some("copied displayed costs"),
        costs: &terms,
        ..AdminLibrarySearchScope::empty()
    };
    let searched = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope,
            filters: &no_filters,
            required_binding_purposes: &[],
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Cost, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(
        searched.iter().map(|row| row.id).collect::<std::collections::HashSet<_>>(),
        ids,
        "search hints OR complete constraints, preserving currency/coverage/unpriced identity"
    );
    let facet = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &no_filters,
            required_binding_purposes: &[],
            column: AdminLibraryFacetColumn::Cost,
            facet_search: Some("copied displayed costs"),
            facet_hints: scope,
            cursor: None,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(facet.rows.len(), 2, "column-local typed hints find full displayed cost tuples");
    assert_eq!(facet.rows.iter().map(|row| row.facet_count).sum::<i64>(), 2);
    let currency = [ironrag_contracts::admin::AdminLibraryCostSearch {
        currency_code: Some("EUR".into()),
        ..Default::default()
    }];
    let currency_scope =
        AdminLibrarySearchScope { costs: &currency, ..AdminLibrarySearchScope::empty() };
    let rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: currency_scope,
            filters: &no_filters,
            required_binding_purposes: &[],
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Cost, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![eur],
        "a currency-only search never converts or drops its currency"
    );
    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admin_library_cost_facet_cursor_keeps_equal_amount_decorations() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let second_library = fixture.create_sibling_library(&pool).await?;
    let third_library = fixture.create_sibling_library(&pool).await?;
    let unknown_library = fixture.create_sibling_library(&pool).await?;
    for (id, unpriced) in
        [(fixture.library_id, 0), (second_library, 1), (third_library, 2), (unknown_library, 4)]
    {
        insert_execution_cost(
            &pool,
            fixture.workspace_id,
            id,
            LibraryExecutionCost {
                total_cost: "1.23",
                currency_code: "USD",
                provider_call_count: 4,
                unpriced_provider_call_count: unpriced,
            },
        )
        .await?;
    }
    let filters = AdminLibraryListFilters::default();
    let first = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &filters,
            required_binding_purposes: &[],
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 1,
        },
    )
    .await?;
    assert!(first.has_more);
    let row = &first.rows[0];
    let cursor = AdminLibraryFacetCursorValue::Cost {
        currency: row.value_currency.clone().context("cost currency missing")?,
        amount: row.value_decimal.context("cost amount missing")?,
        coverage: row.value_coverage.clone().context("cost coverage missing")?,
        unpriced_call_count: row.value_number.context("unpriced call count missing")?,
    };
    let second = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &filters,
            required_binding_purposes: &[],
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: Some(&cursor),
            limit: 1,
        },
    )
    .await?;
    assert_eq!(
        second.rows.len(),
        1,
        "equal amount with a different visible decoration remains reachable"
    );
    assert_eq!(second.rows[0].value_number, Some(1));
    let all = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &filters,
            required_binding_purposes: &[],
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(
        all.rows.len(),
        4,
        "each displayed decoration is distinct; unknown is one NULL bucket"
    );
    assert_eq!(all.rows[2].value_number, Some(2));
    assert!(all.rows[3].value_decimal.is_none());
    assert!(all.rows[3].value_currency.is_none());
    assert!(all.rows[3].value_coverage.is_none());
    assert!(all.rows[3].value_number.is_none());
    let numbers = [4];
    let flags = [true];
    let lifecycle = [String::from("active")];
    for (column, hints) in [
        (
            AdminLibraryFacetColumn::Cost,
            AdminLibrarySearchScope { unavailable: true, ..AdminLibrarySearchScope::empty() },
        ),
        (
            AdminLibraryFacetColumn::Calls,
            AdminLibrarySearchScope { numbers: &numbers, ..AdminLibrarySearchScope::empty() },
        ),
        (
            AdminLibraryFacetColumn::Readiness,
            AdminLibrarySearchScope { readiness: &flags, ..AdminLibrarySearchScope::empty() },
        ),
        (
            AdminLibraryFacetColumn::Lifecycle,
            AdminLibrarySearchScope { lifecycle: &lifecycle, ..AdminLibrarySearchScope::empty() },
        ),
    ] {
        let page = catalog_repository::list_admin_library_facet_page(
            &pool,
            AdminLibraryFacetQuery {
                scope: AdminLibrarySearchScope::empty(),
                filters: &filters,
                required_binding_purposes: &[],
                column,
                facet_search: Some("localized displayed label"),
                facet_hints: hints,
                cursor: None,
                limit: 10,
            },
        )
        .await?;
        assert_eq!(
            page.rows.len(),
            1,
            "typed local facet hints must not disappear behind literal label search"
        );
        let row = &page.rows[0];
        if column == AdminLibraryFacetColumn::Cost {
            assert!(row.value_decimal.is_none());
            assert_eq!(row.facet_count, 1);
        } else {
            assert_eq!(row.facet_count, 4, "unpriced costs never erase readable call counts");
        }
    }
    let unavailable =
        AdminLibrarySearchScope { unavailable: true, ..AdminLibrarySearchScope::empty() };
    let rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: unavailable,
            filters: &filters,
            required_binding_purposes: &[],
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Cost, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, unknown_library);
    assert_eq!(rows[0].provider_call_count, 4, "unknown cost does not erase the known call count");
    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admin_library_cost_facet_filter_search_and_currency_scope() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = CatalogRepositoryFixture::create(&pool).await?;
    let sibling = fixture.create_sibling_library(&pool).await?;

    // Main library: raw >2 fraction digits, partial coverage (one unpriced call).
    insert_execution_cost(
        &pool,
        fixture.workspace_id,
        fixture.library_id,
        LibraryExecutionCost {
            total_cost: "1.2345",
            currency_code: "USD",
            provider_call_count: 4,
            unpriced_provider_call_count: 1,
        },
    )
    .await?;
    // Sibling: the same rounded amount in a different currency, complete.
    insert_execution_cost(
        &pool,
        fixture.workspace_id,
        sibling,
        LibraryExecutionCost {
            total_cost: "1.23",
            currency_code: "EUR",
            provider_call_count: 4,
            unpriced_provider_call_count: 0,
        },
    )
    .await?;

    let purposes: Vec<String> = Vec::new();
    let none = AdminLibraryListFilters::default();

    // The whole-table rounded-amount search matches both currencies' rounded rows.
    let costs = [ironrag_contracts::admin::AdminLibraryCostSearch {
        amount: Some("1.23".to_string()),
        ..Default::default()
    }];
    let cost_search = AdminLibrarySearchScope { costs: &costs, ..AdminLibrarySearchScope::empty() };
    let cost_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: cost_search,
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(cost_rows.len(), 2, "rounded search matches every currency carrying 1.23");

    // A cost filter is currency-scoped: {1.23, USD} selects only the USD library.
    let usd_only = AdminLibraryListFilters {
        cost: AdminLibraryFilterValues {
            present: true,
            include_null: false,
            values: vec![ironrag_contracts::admin::AdminLibraryCostValue {
                amount: "1.23".to_string(),
                currency_code: "USD".to_string(),
                coverage: ironrag_contracts::admin::AdminLibraryCostCoverage::Partial,
                unpriced_call_count: 1,
            }],
        },
        ..AdminLibraryListFilters::default()
    };
    let usd_rows = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &usd_only,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(usd_rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![fixture.library_id]);

    // Currency-primary sort: EUR before USD regardless of amount.
    let cost_order = admin_library_rows(
        &pool,
        AdminLibraryPageQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            order: AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Cost, desc: false },
            offset: 0,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(cost_order.first().map(|row| row.id), Some(sibling));
    assert_eq!(cost_order.last().map(|row| row.id), Some(fixture.library_id));

    // The cost facet groups by (currency, rounded amount, coverage, unpriced).
    let cost_facet = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(cost_facet.rows.len(), 2);
    let currencies: Vec<_> =
        cost_facet.rows.iter().filter_map(|row| row.value_currency.as_deref()).collect();
    assert!(currencies.contains(&"EUR") && currencies.contains(&"USD"));
    assert!(cost_facet.rows.iter().any(|row| row.value_coverage.as_deref() == Some("partial")));
    let rounding: Vec<_> = cost_facet.rows.iter().filter_map(|row| row.value_decimal).collect();
    assert!(rounding.iter().all(|value| *value == rust_decimal::Decimal::new(123, 2)));

    let partial_search = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Cost,
            facet_search: Some("partial"),
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 10,
        },
    )
    .await?;
    assert_eq!(partial_search.rows.len(), 1);
    assert_eq!(partial_search.rows[0].value_coverage.as_deref(), Some("partial"));

    // Keyset over currency-primary order: EUR page then USD via the cursor.
    let first = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: None,
            limit: 1,
        },
    )
    .await?;
    assert_eq!(first.rows[0].value_currency.as_deref(), Some("EUR"));
    let cost_cursor = AdminLibraryFacetCursorValue::Cost {
        currency: "EUR".to_string(),
        amount: rust_decimal::Decimal::new(123, 2),
        coverage: "complete".to_string(),
        unpriced_call_count: 0,
    };
    let second = catalog_repository::list_admin_library_facet_page(
        &pool,
        AdminLibraryFacetQuery {
            scope: AdminLibrarySearchScope::empty(),
            filters: &none,
            required_binding_purposes: &purposes,
            column: AdminLibraryFacetColumn::Cost,
            facet_search: None,
            facet_hints: AdminLibrarySearchScope::empty(),

            cursor: Some(&cost_cursor),
            limit: 1,
        },
    )
    .await?;
    assert_eq!(second.rows[0].value_currency.as_deref(), Some("USD"));

    fixture.cleanup(&pool).await?;
    pool.cleanup().await?;
    Ok(())
}
