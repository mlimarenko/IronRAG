#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::Context;
use chrono::{TimeZone, Utc};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        content_repository,
        content_repository::NewContentMutation,
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
                url_filter_mode: "blocklist",
                url_patterns: json!([]),
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

        assert_eq!(counts.discovered, 10);
        assert_eq!(counts.eligible, 6);
        assert_eq!(counts.processed, 1);
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.processing, 1);
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
        let cancel_requested_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).single().unwrap();
        let completed_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 5, 0).single().unwrap();

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
