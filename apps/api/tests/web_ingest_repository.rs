use anyhow::Context;
use chrono::{TimeZone, Utc};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        content_repository,
        content_repository::{
            NewContentDocument, NewContentMutation, NewContentMutationItem, NewContentRevision,
        },
        iam_repository, ingest_repository,
        ingest_repository::{
            NewWebDiscoveredPage, NewWebIngestRun, UpdateWebIngestRun,
            get_web_discovered_page_by_run_and_normalized_url, get_web_run_counts,
        },
    },
};

struct WebIngestRepositoryFixture {
    principal_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl WebIngestRepositoryFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let principal =
            iam_repository::create_principal(pool, "user", "Web Ingest Repo Test", None)
                .await
                .context("failed to create web ingest repository principal")?;
        let workspace_id = sqlx::query_scalar::<_, Uuid>(
            "insert into catalog_workspace (
                id,
                slug,
                display_name,
                lifecycle_state,
                created_by_principal_id,
                created_at,
                updated_at
            )
            values ($1, $2, $3, 'active', $4, now(), now())
            returning id",
        )
        .bind(Uuid::now_v7())
        .bind(format!("web-ingest-repo-{suffix}"))
        .bind("Web Ingest Repository Test Workspace")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert web ingest repository workspace")?;
        let library_id = sqlx::query_scalar::<_, Uuid>(
            "insert into catalog_library (
                id,
                workspace_id,
                slug,
                display_name,
                description,
                lifecycle_state,
                created_by_principal_id,
                created_at,
                updated_at
            )
            values ($1, $2, $3, $4, $5, 'active', $6, now(), now())
            returning id",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(format!("web-ingest-library-{suffix}"))
        .bind("Web Ingest Repository Test Library")
        .bind("canonical web ingest repository tests")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert web ingest repository library")?;

        Ok(Self { principal_id: principal.id, workspace_id, library_id })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from catalog_workspace where id = $1")
            .bind(self.workspace_id)
            .execute(pool)
            .await
            .context("failed to delete web ingest repository workspace")?;
        sqlx::query("delete from iam_principal where id = $1")
            .bind(self.principal_id)
            .execute(pool)
            .await
            .context("failed to delete web ingest repository principal")?;
        Ok(())
    }

    async fn create_mutation(&self, pool: &PgPool, suffix: &str) -> anyhow::Result<Uuid> {
        let mutation = content_repository::create_mutation(
            pool,
            &NewContentMutation {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                operation_kind: "web_capture",
                requested_by_principal_id: Some(self.principal_id),
                request_surface: "rest",
                idempotency_key: Some(suffix),
                source_identity: Some(suffix),
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .with_context(|| format!("failed to create web ingest mutation {suffix}"))?;
        Ok(mutation.id)
    }

    async fn create_run(
        &self,
        pool: &PgPool,
        suffix: &str,
        mode: &str,
        boundary_policy: &str,
        max_depth: i32,
        max_pages: i32,
    ) -> anyhow::Result<ingest_repository::WebIngestRunRow> {
        let mutation_id = self.create_mutation(pool, suffix).await?;
        ingest_repository::create_web_ingest_run(
            pool,
            &NewWebIngestRun {
                id: Uuid::now_v7(),
                mutation_id,
                async_operation_id: None,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                mode,
                seed_url: "https://example.com/seed",
                normalized_seed_url: "https://example.com/seed",
                boundary_policy,
                max_depth,
                max_pages,
                crawl_allow_patterns: json!([]),
                crawl_block_patterns: json!([]),
                materialization_allow_patterns: json!([]),
                materialization_block_patterns: json!([]),
                run_state: "accepted",
                requested_by_principal_id: Some(self.principal_id),
                requested_at: None,
                completed_at: None,
                failure_code: None,
                cancel_requested_at: None,
            },
        )
        .await
        .with_context(|| format!("failed to create web ingest run {suffix}"))
    }

    async fn create_page(
        &self,
        pool: &PgPool,
        run_id: Uuid,
        normalized_url: &str,
        canonical_url: Option<&str>,
        candidate_state: &str,
        classification_reason: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let page = ingest_repository::create_web_discovered_page(
            pool,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id,
                discovered_url: Some(normalized_url),
                normalized_url,
                final_url: canonical_url,
                canonical_url,
                depth: 0,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state,
                classification_reason,
                classification_detail: None,
                content_type: Some("text/html"),
                http_status: Some(200),
                snapshot_storage_key: None,
                discovered_at: None,
                updated_at: None,
                document_id: None,
                result_revision_id: None,
                mutation_item_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create web discovered page {normalized_url}"))?;
        Ok(page.id)
    }

    async fn create_materialized_page(
        &self,
        pool: &PgPool,
        run_id: Uuid,
        mutation_id: Uuid,
        suffix: &str,
        item_state: &str,
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let external_key = format!("https://example.test/{suffix}");
        let document = content_repository::create_document(
            pool,
            &NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                external_key: &external_key,
                document_state: "active",
                created_by_principal_id: Some(self.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .with_context(|| format!("failed to create settlement document {suffix}"))?;
        let checksum = format!("sha256:{suffix}");
        let revision = content_repository::create_revision(
            pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "web_page",
                checksum: &checksum,
                mime_type: "text/plain",
                byte_size: 1,
                title: Some("Settlement fixture"),
                language_code: None,
                source_uri: Some(&external_key),
                document_hint: None,
                storage_key: None,
                created_by_principal_id: Some(self.principal_id),
            },
        )
        .await
        .with_context(|| format!("failed to create settlement revision {suffix}"))?;
        let mutation_item = content_repository::create_mutation_item(
            pool,
            &NewContentMutationItem {
                mutation_id,
                document_id: Some(document.id),
                base_revision_id: None,
                result_revision_id: Some(revision.id),
                item_state,
                message: None,
            },
        )
        .await
        .with_context(|| format!("failed to create settlement mutation item {suffix}"))?;
        let page = ingest_repository::create_web_discovered_page(
            pool,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id,
                discovered_url: Some(&external_key),
                normalized_url: &external_key,
                final_url: Some(&external_key),
                canonical_url: Some(&external_key),
                depth: 0,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state: "materialized",
                classification_reason: Some("seed_accepted"),
                classification_detail: None,
                content_type: Some("text/plain"),
                http_status: Some(200),
                snapshot_storage_key: None,
                discovered_at: None,
                updated_at: None,
                document_id: Some(document.id),
                result_revision_id: Some(revision.id),
                mutation_item_id: Some(mutation_item.id),
            },
        )
        .await
        .with_context(|| format!("failed to create materialized page {suffix}"))?;
        Ok((page.id, mutation_item.id))
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect web ingest repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for web ingest repository test")?;
    Ok(pool)
}

fn database_error_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|database_error| database_error.code().map(std::borrow::Cow::into_owned))
}

fn anyhow_database_error_code(error: &anyhow::Error) -> Option<String> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<sqlx::Error>().and_then(database_error_code))
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_ingest_run_repository_enforces_constraints_and_keeps_settings_immutable()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(&pool, "settings", "recursive_crawl", "allow_external", 3, 25)
            .await?;

        let updated = ingest_repository::update_web_ingest_run(
            &pool,
            run.id,
            &UpdateWebIngestRun {
                run_state: "processing",
                completed_at: None,
                failure_code: None,
                cancel_requested_at: None,
            },
        )
        .await
        .context("failed to update web ingest run")?
        .context("missing updated web ingest run")?;
        let reloaded = ingest_repository::get_web_ingest_run_by_id(&pool, run.id)
            .await
            .context("failed to reload web ingest run")?
            .context("missing reloaded web ingest run")?;

        assert_eq!(updated.mode, "recursive_crawl");
        assert_eq!(updated.boundary_policy, "allow_external");
        assert_eq!(updated.max_depth, 3);
        assert_eq!(updated.max_pages, 25);
        assert_eq!(reloaded.seed_url, "https://example.com/seed");
        assert_eq!(reloaded.normalized_seed_url, "https://example.com/seed");
        assert_eq!(reloaded.mode, "recursive_crawl");
        assert_eq!(reloaded.boundary_policy, "allow_external");
        assert_eq!(reloaded.max_depth, 3);
        assert_eq!(reloaded.max_pages, 25);
        assert_eq!(reloaded.run_state, "processing");

        let negative_depth_error = fixture
            .create_run(&pool, "negative-depth", "recursive_crawl", "same_host", -1, 10)
            .await
            .expect_err("negative max_depth must violate migration check");
        assert_eq!(anyhow_database_error_code(&negative_depth_error).as_deref(), Some("23514"));

        let zero_pages_error = fixture
            .create_run(&pool, "zero-pages", "recursive_crawl", "same_host", 1, 0)
            .await
            .expect_err("zero max_pages must violate migration check");
        assert_eq!(anyhow_database_error_code(&zero_pages_error).as_deref(), Some("23514"));

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_discovered_page_repository_allows_canonical_duplicates_and_rolls_up_counts()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run =
            fixture.create_run(&pool, "counts", "recursive_crawl", "same_host", 3, 25).await?;

        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/eligible",
                Some("https://example.com/eligible"),
                "eligible",
                Some("seed_accepted"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/queued",
                Some("https://example.com/queued"),
                "queued",
                Some("seed_accepted"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/processing",
                Some("https://example.com/processing"),
                "processing",
                Some("seed_accepted"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/materialized",
                Some("https://example.com/materialized"),
                "materialized",
                Some("seed_accepted"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/processed",
                Some("https://example.com/processed"),
                "processed",
                Some("seed_accepted"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/failed",
                Some("https://example.com/failed"),
                "failed",
                Some("unsupported_content"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/canceled",
                Some("https://example.com/canceled"),
                "canceled",
                Some("cancel_requested"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/duplicate",
                Some("https://example.com/duplicate"),
                "duplicate",
                Some("duplicate_canonical_url"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/excluded",
                Some("https://example.com/excluded"),
                "excluded",
                Some("outside_boundary_policy"),
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                "https://example.com/blocked",
                Some("https://example.com/blocked"),
                "blocked",
                Some("inaccessible"),
            )
            .await?;

        let duplicate_alias = ingest_repository::create_web_discovered_page(
            &pool,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id: run.id,
                discovered_url: Some("https://example.com/duplicate-alias"),
                normalized_url: "https://example.com/duplicate-alias",
                final_url: Some("https://example.com/duplicate"),
                canonical_url: Some("https://example.com/duplicate"),
                depth: 1,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state: "duplicate",
                classification_reason: Some("duplicate_canonical_url"),
                classification_detail: None,
                content_type: Some("text/html"),
                http_status: Some(200),
                snapshot_storage_key: None,
                discovered_at: None,
                updated_at: None,
                document_id: None,
                result_revision_id: None,
                mutation_item_id: None,
            },
        )
        .await
        .context("duplicate canonical url alias should persist")?;

        let counts =
            get_web_run_counts(&pool, run.id).await.context("failed to load web run counts")?;
        let queued_page = get_web_discovered_page_by_run_and_normalized_url(
            &pool,
            run.id,
            "https://example.com/queued",
        )
        .await
        .context("failed to load queued page by normalized url")?
        .context("missing queued page")?;

        assert_eq!(counts.discovered, 11);
        assert_eq!(counts.eligible, 7);
        assert_eq!(counts.processed, 1);
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.processing, 2);
        assert_eq!(counts.duplicates, 2);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.blocked, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.canceled, 1);
        assert!(counts.last_activity_at.is_some());
        assert_eq!(duplicate_alias.canonical_url.as_deref(), Some("https://example.com/duplicate"));
        assert_eq!(duplicate_alias.candidate_state, "duplicate");
        assert_eq!(queued_page.candidate_state, "queued");
        assert_eq!(queued_page.classification_reason.as_deref(), Some("seed_accepted"));

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_ingest_run_repository_persists_cancellation_markers() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run =
            fixture.create_run(&pool, "cancel", "recursive_crawl", "same_host", 2, 20).await?;
        let cancel_requested_at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .context("invalid cancel-request fixture timestamp")?;
        let completed_at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 5, 0)
            .single()
            .context("invalid completion fixture timestamp")?;

        let canceled = ingest_repository::update_web_ingest_run(
            &pool,
            run.id,
            &UpdateWebIngestRun {
                run_state: "canceled",
                completed_at: Some(completed_at),
                failure_code: None,
                cancel_requested_at: Some(cancel_requested_at),
            },
        )
        .await
        .context("failed to persist canceled web ingest run")?
        .context("missing canceled web ingest run")?;
        let reloaded_by_id = ingest_repository::get_web_ingest_run_by_id(&pool, run.id)
            .await
            .context("failed to reload canceled web ingest run by id")?
            .context("missing canceled web ingest run by id")?;
        let reloaded_by_mutation =
            ingest_repository::get_web_ingest_run_by_mutation_id(&pool, run.mutation_id)
                .await
                .context("failed to reload canceled web ingest run by mutation")?
                .context("missing canceled web ingest run by mutation")?;

        assert_eq!(canceled.run_state, "canceled");
        assert_eq!(canceled.cancel_requested_at, Some(cancel_requested_at));
        assert_eq!(canceled.completed_at, Some(completed_at));
        assert_eq!(reloaded_by_id.cancel_requested_at, Some(cancel_requested_at));
        assert_eq!(reloaded_by_id.completed_at, Some(completed_at));
        assert_eq!(reloaded_by_mutation.id, run.id);
        assert_eq!(reloaded_by_mutation.run_state, "canceled");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_page_settlement_waits_for_its_exact_mutation_item_before_completing_run()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest settlement test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture.create_run(&pool, "settlement", "single_page", "same_host", 0, 1).await?;
        ingest_repository::update_web_ingest_run(
            &pool,
            run.id,
            &UpdateWebIngestRun {
                run_state: "processing",
                completed_at: None,
                failure_code: None,
                cancel_requested_at: None,
            },
        )
        .await
        .context("failed to start settlement run")?;
        let (page_id, mutation_item_id) = fixture
            .create_materialized_page(&pool, run.id, run.mutation_id, "pending", "pending")
            .await?;

        let linked_page = ingest_repository::get_web_discovered_page_by_id(&pool, page_id)
            .await?
            .context("linked settlement page disappeared")?;
        ingest_repository::update_web_discovered_page(
            &pool,
            page_id,
            &ingest_repository::UpdateWebDiscoveredPage {
                final_url: linked_page.final_url.as_deref(),
                canonical_url: linked_page.canonical_url.as_deref(),
                host_classification: Some(linked_page.host_classification.as_str()),
                candidate_state: "materialized",
                classification_reason: linked_page.classification_reason.as_deref(),
                classification_detail: linked_page.classification_detail.as_deref(),
                content_type: linked_page.content_type.as_deref(),
                http_status: linked_page.http_status,
                snapshot_storage_key: linked_page.snapshot_storage_key.as_deref(),
                updated_at: None,
                document_id: linked_page.document_id,
                result_revision_id: linked_page.result_revision_id,
                mutation_item_id: None,
            },
        )
        .await?
        .context("settlement page disappeared while simulating pre-link callback")?;
        let pre_link =
            ingest_repository::settle_web_ingest_mutation_item(&pool, mutation_item_id, Utc::now())
                .await?;
        assert!(matches!(
            pre_link,
            ingest_repository::SettleWebIngestMutationItemOutcome::AwaitingLink {
                mutation_item_id: observed_item_id,
            } if observed_item_id == mutation_item_id
        ));
        ingest_repository::update_web_discovered_page(
            &pool,
            page_id,
            &ingest_repository::UpdateWebDiscoveredPage {
                final_url: linked_page.final_url.as_deref(),
                canonical_url: linked_page.canonical_url.as_deref(),
                host_classification: Some(linked_page.host_classification.as_str()),
                candidate_state: "materialized",
                classification_reason: linked_page.classification_reason.as_deref(),
                classification_detail: linked_page.classification_detail.as_deref(),
                content_type: linked_page.content_type.as_deref(),
                http_status: linked_page.http_status,
                snapshot_storage_key: linked_page.snapshot_storage_key.as_deref(),
                updated_at: None,
                document_id: linked_page.document_id,
                result_revision_id: linked_page.result_revision_id,
                mutation_item_id: Some(mutation_item_id),
            },
        )
        .await?
        .context("settlement page disappeared while restoring exact item link")?;

        let waiting =
            ingest_repository::settle_web_ingest_mutation_item(&pool, mutation_item_id, Utc::now())
                .await
                .context("failed to inspect pending web page settlement")?;
        assert!(matches!(
            waiting,
            ingest_repository::SettleWebIngestMutationItemOutcome::AwaitingItem {
                page_id: observed_page_id,
                run_id: observed_run_id,
            } if observed_page_id == page_id && observed_run_id == run.id
        ));
        let pending_page = ingest_repository::get_web_discovered_page_by_id(&pool, page_id)
            .await?
            .context("pending settlement page disappeared")?;
        let pending_run = ingest_repository::get_web_ingest_run_by_id(&pool, run.id)
            .await?
            .context("pending settlement run disappeared")?;
        let pending_mutation = content_repository::get_mutation_by_id(&pool, run.mutation_id)
            .await?
            .context("pending settlement mutation disappeared")?;
        assert_eq!(pending_page.candidate_state, "materialized");
        assert_eq!(pending_run.run_state, "processing");
        assert_eq!(pending_mutation.mutation_state, "accepted");

        let item = content_repository::get_mutation_item_by_id(&pool, mutation_item_id)
            .await?
            .context("pending settlement item disappeared")?;
        content_repository::update_mutation_item(
            &pool,
            item.id,
            item.document_id,
            item.base_revision_id,
            item.result_revision_id,
            "applied",
            None,
        )
        .await?
        .context("applied settlement item disappeared")?;

        let settled =
            ingest_repository::settle_web_ingest_mutation_item(&pool, mutation_item_id, Utc::now())
                .await
                .context("failed to settle applied web page")?;
        assert!(matches!(
            settled,
            ingest_repository::SettleWebIngestMutationItemOutcome::Settled {
                page_id: observed_page_id,
                run_id: observed_run_id,
                ref candidate_state,
                ref run_state,
                run_completed: true,
            } if observed_page_id == page_id
                && observed_run_id == run.id
                && candidate_state == "processed"
                && run_state == "completed"
        ));
        let settled_page = ingest_repository::get_web_discovered_page_by_id(&pool, page_id)
            .await?
            .context("settled page disappeared")?;
        let settled_run = ingest_repository::get_web_ingest_run_by_id(&pool, run.id)
            .await?
            .context("settled run disappeared")?;
        let settled_mutation = content_repository::get_mutation_by_id(&pool, run.mutation_id)
            .await?
            .context("settled mutation disappeared")?;
        assert_eq!(settled_page.candidate_state, "processed");
        assert_eq!(settled_run.run_state, "completed");
        assert!(settled_run.completed_at.is_some());
        assert_eq!(settled_mutation.mutation_state, "applied");
        assert!(settled_mutation.completed_at.is_some());

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_page_settlement_handles_terminal_failure_without_first_item_shortcuts()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for web ingest failure settlement test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(&pool, "partial-settlement", "recursive_crawl", "same_host", 1, 2)
            .await?;
        ingest_repository::update_web_ingest_run(
            &pool,
            run.id,
            &UpdateWebIngestRun {
                run_state: "processing",
                completed_at: None,
                failure_code: None,
                cancel_requested_at: None,
            },
        )
        .await
        .context("failed to start partial settlement run")?;
        let (applied_page_id, applied_item_id) = fixture
            .create_materialized_page(&pool, run.id, run.mutation_id, "applied-child", "applied")
            .await?;
        let (failed_page_id, failed_item_id) = fixture
            .create_materialized_page(&pool, run.id, run.mutation_id, "failed-child", "pending")
            .await?;

        let first =
            ingest_repository::settle_web_ingest_mutation_item(&pool, applied_item_id, Utc::now())
                .await?;
        assert!(matches!(
            first,
            ingest_repository::SettleWebIngestMutationItemOutcome::Settled {
                page_id,
                run_completed: false,
                ..
            } if page_id == applied_page_id
        ));
        let in_flight_run = ingest_repository::get_web_ingest_run_by_id(&pool, run.id)
            .await?
            .context("partial settlement run disappeared")?;
        assert_eq!(in_flight_run.run_state, "processing");

        let failed_item = content_repository::get_mutation_item_by_id(&pool, failed_item_id)
            .await?
            .context("failed settlement item disappeared")?;
        content_repository::update_mutation_item(
            &pool,
            failed_item.id,
            failed_item.document_id,
            failed_item.base_revision_id,
            failed_item.result_revision_id,
            "failed",
            Some("terminal child failure"),
        )
        .await?
        .context("terminal settlement item disappeared")?;
        let terminal =
            ingest_repository::settle_web_ingest_mutation_item(&pool, failed_item_id, Utc::now())
                .await?;
        assert!(matches!(
            terminal,
            ingest_repository::SettleWebIngestMutationItemOutcome::Settled {
                page_id,
                ref candidate_state,
                ref run_state,
                run_completed: true,
                ..
            } if page_id == failed_page_id
                && candidate_state == "failed"
                && run_state == "completed_partial"
        ));
        let terminal_mutation = content_repository::get_mutation_by_id(&pool, run.mutation_id)
            .await?
            .context("partial terminal mutation disappeared")?;
        assert_eq!(terminal_mutation.mutation_state, "applied");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
