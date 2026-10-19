use crate::temp_postgres_database;

use anyhow::Context;
use chrono::{TimeZone, Utc};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        admission_repository, catalog_repository, content_mutation_repository, iam_repository,
        ingest_repository,
        ingest_repository::{
            NewWebDiscoveredPage, UpdateWebIngestRun,
            get_web_discovered_page_by_run_and_normalized_url, get_web_run_counts,
        },
    },
};

struct WebIngestRepositoryFixture {
    principal_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
}

/// One web run's settings: the mutation suffix plus the crawl mode, boundary
/// policy and depth/page bounds the migration validates.
struct WebRunSeed<'a> {
    suffix: &'a str,
    mode: &'a str,
    boundary_policy: &'a str,
    max_depth: i32,
    max_pages: i32,
}

/// One discovered-page row: its normalized/preferred URLs, candidate state and
/// classification reason.
struct DiscoveredPageSeed<'a> {
    normalized_url: &'a str,
    preferred_url: Option<&'a str>,
    candidate_state: &'a str,
    classification_reason: Option<&'a str>,
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
        .bind("authoritative web ingest repository tests")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert web ingest repository library")?;

        Ok(Self { principal_id: principal.id, workspace_id, library_id })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        // See `content_repository`: mutation items hold an ON DELETE RESTRICT
        // reference to their document, so only the repository's delete order
        // can remove a workspace that has run a mutation.
        catalog_repository::delete_workspace(pool, self.workspace_id)
            .await
            .context("failed to delete web ingest repository workspace")?;
        sqlx::query("delete from iam_principal where id = $1")
            .bind(self.principal_id)
            .execute(pool)
            .await
            .context("failed to delete web ingest repository principal")?;
        Ok(())
    }

    async fn create_run(
        &self,
        pool: &PgPool,
        seed: WebRunSeed<'_>,
    ) -> anyhow::Result<ingest_repository::WebIngestRunRow> {
        let bundle = admission_repository::admit_web_run_with_failpoint(
            pool,
            &admission_repository::WebRunAdmissionRequest {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                seed_url: "https://example.com/seed".to_owned(),
                normalized_seed_url: "https://example.com/seed".to_owned(),
                mode: seed.mode.to_owned(),
                boundary_policy: seed.boundary_policy.to_owned(),
                max_depth: seed.max_depth,
                max_pages: seed.max_pages,
                crawl_allow_patterns: json!([]),
                crawl_block_patterns: json!([]),
                materialization_allow_patterns: json!([]),
                materialization_block_patterns: json!([]),
                requested_by_principal_id: Some(self.principal_id),
                request_surface: "rest".to_owned(),
                idempotency_key: Some(seed.suffix.to_owned()),
                source_identity: Some(seed.suffix.to_owned()),
            },
            None,
        )
        .await?
        .into_bundle();
        // These repository cases manually supply their observed pages. Keep the
        // canonical seed but exclude it from the in-flight settlement inventory.
        sqlx::query(
            "update content_web_discovered_page set candidate_state = 'excluded',
                     classification_reason = 'outside_boundary_policy' where id = $1",
        )
        .bind(bundle.seed.id)
        .execute(pool)
        .await?;
        Ok(bundle.run)
    }

    async fn create_page(
        &self,
        pool: &PgPool,
        run_id: Uuid,
        seed: DiscoveredPageSeed<'_>,
    ) -> anyhow::Result<Uuid> {
        let page = ingest_repository::create_web_discovered_page(
            pool,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id,
                discovered_url: Some(seed.normalized_url),
                normalized_url: seed.normalized_url,
                final_url: seed.preferred_url,
                preferred_url: seed.preferred_url,
                depth: 0,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state: seed.candidate_state,
                classification_reason: seed.classification_reason,
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
        .with_context(|| format!("failed to create web discovered page {}", seed.normalized_url))?;
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
        let run = ingest_repository::get_web_ingest_run_by_id(pool, run_id)
            .await?
            .context("materialized fixture run missing")?;
        let external_key = if run.mode == "single_page" {
            run.normalized_seed_url.clone()
        } else {
            format!("https://example.test/{suffix}")
        };
        let request = web_page_request(self, mutation_id, &external_key, suffix);
        let admission = admission_repository::admit_web_capture_materialization_with_failpoint(
            pool, &request, None,
        )
        .await?
        .into_bundle();
        let admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(bundle) =
            admission
        else {
            anyhow::bail!("settlement fixture unexpectedly deduplicated");
        };
        let document = bundle.document;
        let revision = bundle.revision.context("web fixture revision missing")?;
        let mutation_item = bundle.item.context("web fixture item missing")?;
        if item_state != "pending" {
            content_mutation_repository::update_mutation_item(
                pool,
                content_mutation_repository::MutationItemUpdate {
                    item_id: mutation_item.id,
                    document_id: Some(document.id),
                    base_revision_id: None,
                    result_revision_id: Some(revision.id),
                    item_state,
                    message: None,
                },
            )
            .await?;
        }
        if run.mode == "single_page" {
            // A single-page observation materializes the accepted seed. Adding
            // another page leaves an artificial excluded seed in the aggregate
            // and correctly classifies an otherwise successful run as partial.
            let seed = get_web_discovered_page_by_run_and_normalized_url(
                pool,
                run_id,
                &run.normalized_seed_url,
            )
            .await?
            .context("single-page fixture seed missing")?;
            let page = ingest_repository::update_web_discovered_page(
                pool,
                seed.id,
                &ingest_repository::UpdateWebDiscoveredPage {
                    final_url: Some(&external_key),
                    preferred_url: Some(&external_key),
                    host_classification: Some("same_host"),
                    candidate_state: "materialized",
                    classification_reason: Some("seed_accepted"),
                    classification_detail: None,
                    content_type: Some("text/plain"),
                    http_status: Some(200),
                    snapshot_storage_key: Some(&request.storage_key),
                    updated_at: None,
                    document_id: Some(document.id),
                    result_revision_id: Some(revision.id),
                    mutation_item_id: Some(mutation_item.id),
                },
            )
            .await?
            .context("single-page fixture seed disappeared")?;
            return Ok((page.id, mutation_item.id));
        }
        let page = ingest_repository::create_web_discovered_page(
            pool,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id,
                discovered_url: Some(&external_key),
                normalized_url: &external_key,
                final_url: Some(&external_key),
                preferred_url: Some(&external_key),
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

fn web_page_request(
    fixture: &WebIngestRepositoryFixture,
    mutation_id: Uuid,
    url: &str,
    body: &str,
) -> admission_repository::WebCaptureMaterializationAdmissionRequest {
    use sha2::{Digest as _, Sha256};
    admission_repository::WebCaptureMaterializationAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        mutation_id,
        final_url: url.to_owned(),
        replacement: ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes()))),
            byte_size: i64::try_from(body.len()).expect("fixture fits i64"),
            mime_type: "text/plain".to_owned(),
            file_name: "snapshot.txt".to_owned(),
            title: Some("Settlement fixture".to_owned()),
            language_code: None,
            source_uri: Some(url.to_owned()),
            document_hint: None,
        },
        storage_key: format!("web-fixture/{mutation_id}/snapshot.txt"),
        requested_by_principal_id: Some(fixture.principal_id),
        priority: 100,
    }
}

async fn connect_postgres(
    settings: &Settings,
) -> anyhow::Result<(PgPool, temp_postgres_database::TempPostgresDatabase)> {
    let temp_database = temp_postgres_database::TempPostgresDatabase::create(
        &settings.database_url,
        "web_ingest_repository",
    )
    .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect web ingest repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for web ingest repository test")?;
    Ok((pool, temp_database))
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
async fn web_materialization_child_identity_replays_without_rebinding_parent() -> anyhow::Result<()>
{
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;
    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "owning-parent",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: 1,
                    max_pages: 10,
                },
            )
            .await?;
        let request = web_page_request(&fixture, run.mutation_id, "https://example.com/one", "one");
        let parent_identity: serde_json::Value =
            sqlx::query_scalar("select request_identity from content_mutation where id = $1")
                .bind(run.mutation_id)
                .fetch_one(&pool)
                .await?;
        let created = admission_repository::admit_web_capture_materialization_with_failpoint(
            &pool, &request, None,
        )
        .await?;
        assert!(matches!(created, admission_repository::AdmissionOutcome::Created(_)));
        let admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(first) =
            created.into_bundle()
        else {
            anyhow::bail!("expected payload admission");
        };
        assert_ne!(first.mutation.id, run.mutation_id);
        assert_eq!(first.async_operation.parent_async_operation_id, run.async_operation_id);
        let stored: serde_json::Value =
            sqlx::query_scalar("select request_identity from content_mutation where id = $1")
                .bind(first.mutation.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored["replacement"], serde_json::to_value(&request.replacement)?);
        assert_eq!(stored["replacement"]["fileName"], "snapshot.txt");
        let mut retransmission = request.clone();
        retransmission.storage_key = "web-fixture/another-physical-location.txt".to_owned();
        let replay = admission_repository::admit_web_capture_materialization_with_failpoint(
            &pool,
            &retransmission,
            None,
        )
        .await?;
        assert!(matches!(replay, admission_repository::AdmissionOutcome::Replayed(_)));
        let admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(replay) =
            replay.into_bundle()
        else {
            anyhow::bail!("expected original payload owner");
        };
        assert_eq!(first.mutation.id, replay.mutation.id);
        assert_eq!(
            first.item.as_ref().map(|item| item.id),
            replay.item.as_ref().map(|item| item.id)
        );
        assert_eq!(first.job.as_ref().map(|job| job.id), replay.job.as_ref().map(|job| job.id));
        assert_eq!(
            first.revision.as_ref().map(|revision| revision.id),
            replay.revision.as_ref().map(|revision| revision.id)
        );
        let second_request =
            web_page_request(&fixture, run.mutation_id, "https://example.com/two", "two");
        let admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(second) =
            admission_repository::admit_web_capture_materialization_with_failpoint(
                &pool,
                &second_request,
                None,
            )
            .await?
            .into_bundle()
        else {
            anyhow::bail!("expected second page owner");
        };
        assert_ne!(first.mutation.id, second.mutation.id);
        assert_eq!(second.async_operation.parent_async_operation_id, run.async_operation_id);
        let unchanged: serde_json::Value =
            sqlx::query_scalar("select request_identity from content_mutation where id = $1")
                .bind(run.mutation_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(unchanged, parent_identity);
        retransmission.replacement.file_name = "changed.txt".to_owned();
        assert!(matches!(
            admission_repository::admit_web_capture_materialization_with_failpoint(
                &pool,
                &retransmission,
                None,
            )
            .await,
            Err(admission_repository::AdmissionError::IdempotencyConflict { .. })
        ));
        Ok::<(), anyhow::Error>(())
    }
    .await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_materialization_rolls_back_every_canonical_child_stage() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;
    let result = async {
        let run = fixture.create_run(&pool, WebRunSeed {
            suffix: "rollback-parent", mode: "recursive_crawl", boundary_policy: "same_host",
            max_depth: 1, max_pages: 10,
        }).await?;
        use admission_repository::AdmissionFailpoint as F;
        for stage in [F::AfterMutation, F::AfterDocument, F::AfterAsyncOperation, F::AfterRevision,
            F::AfterMutationItem, F::AfterJob, F::AfterHead, F::BeforeCommit] {
            let request = web_page_request(&fixture, run.mutation_id, "https://example.com/rollback", "snapshot");
            let error = admission_repository::admit_web_capture_materialization_with_failpoint(
                &pool, &request, Some(stage),
            ).await.expect_err("stage must roll back");
            assert!(matches!(error, admission_repository::AdmissionError::InjectedFailure(actual) if actual == stage));
            let rows: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
                "select
                 (select count(*) from content_mutation where library_id = $1),
                 (select count(*) from content_document where library_id = $1),
                 (select count(*) from content_revision where library_id = $1),
                 (select count(*) from knowledge_document where library_id = $1),
                 (select count(*) from ingest_job where library_id = $1),
                 (select count(*) from ops_async_operation where library_id = $1)",
            ).bind(fixture.library_id).fetch_one(&pool).await?;
            assert_eq!(rows, (1, 0, 0, 0, 1, 1), "{stage:?}");
        }
        Ok::<(), anyhow::Error>(())
    }.await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_materialization_denial_preserves_foreign_scope_and_current_intent()
-> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;
    let foreign = WebIngestRepositoryFixture::create(&pool).await?;
    let result = async {
        let run = fixture.create_run(&pool, WebRunSeed {
            suffix: "intent-parent", mode: "recursive_crawl", boundary_policy: "same_host",
            max_depth: 1, max_pages: 10,
        }).await?;
        let request = web_page_request(&fixture, run.mutation_id, "https://example.com/intent", "snapshot");
        let mut cross_scope = request.clone();
        cross_scope.workspace_id = foreign.workspace_id;
        cross_scope.library_id = foreign.library_id;
        assert!(admission_repository::admit_web_capture_materialization_with_failpoint(&pool, &cross_scope, None).await.is_err());
        let mut wrong_actor = request.clone();
        wrong_actor.requested_by_principal_id = Some(foreign.principal_id);
        assert!(admission_repository::admit_web_capture_materialization_with_failpoint(&pool, &wrong_actor, None).await.is_err());
        let mut wrong_source = request.clone();
        wrong_source.replacement.source_uri = Some("https://example.com/unrelated".to_owned());
        assert!(admission_repository::admit_web_capture_materialization_with_failpoint(&pool, &wrong_source, None).await.is_err());
        let admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(first) =
            admission_repository::admit_web_capture_materialization_with_failpoint(&pool, &request, None)
                .await?.into_bundle() else { anyhow::bail!("expected page owner"); };
        let receipt = admission_repository::admit_visibility_intent_with_failpoint(
            &pool,
            &admission_repository::VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id,
                document_id: first.document.id, requested_by_principal_id: Some(fixture.principal_id),
                request_surface: "rest".to_owned(),
                intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                    idempotency_key: "web-independent-denial".to_owned(), source_identity: "web-observation".to_owned(),
                    materialization_token: None, replacement: request.replacement.clone(),
                },
            }, None,
        ).await?.into_bundle().receipt;
        let later_run = fixture.create_run(&pool, WebRunSeed {
            suffix: "later-parent", mode: "recursive_crawl", boundary_policy: "same_host",
            max_depth: 1, max_pages: 10,
        }).await?;
        let mut later = request.clone();
        later.mutation_id = later_run.mutation_id;
        assert!(matches!(admission_repository::admit_web_capture_materialization_with_failpoint(&pool, &later, None).await,
            Err(admission_repository::AdmissionError::VisibilityIntentRequired { document_id }) if document_id == first.document.id));
        let head: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select visibility_intent_id, latest_mutation_id from content_document_head where document_id = $1",
        ).bind(first.document.id).fetch_one(&pool).await?;
        assert_eq!(head, (Some(receipt.visibility_intent_id), Some(first.mutation.id)));
        let owner_state: String = sqlx::query_scalar("select mutation_state::text from content_mutation where id = $1")
            .bind(first.mutation.id).fetch_one(&pool).await?;
        assert_eq!(owner_state, "accepted");
        let run_cancelled: bool = sqlx::query_scalar("select cancel_requested_at is not null from content_web_ingest_run where id = $1")
            .bind(later_run.id).fetch_one(&pool).await?;
        assert!(!run_cancelled);
        Ok::<(), anyhow::Error>(())
    }.await;
    foreign.cleanup(&pool).await?;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_ingest_run_repository_enforces_constraints_and_keeps_settings_immutable()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "settings",
                    mode: "recursive_crawl",
                    boundary_policy: "allow_external",
                    max_depth: 3,
                    max_pages: 25,
                },
            )
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
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "negative-depth",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: -1,
                    max_pages: 10,
                },
            )
            .await
            .expect_err("negative max_depth must violate migration check");
        assert_eq!(anyhow_database_error_code(&negative_depth_error).as_deref(), Some("23514"));

        let zero_pages_error = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "zero-pages",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: 1,
                    max_pages: 0,
                },
            )
            .await
            .expect_err("zero max_pages must violate migration check");
        assert_eq!(anyhow_database_error_code(&zero_pages_error).as_deref(), Some("23514"));

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_discovered_page_repository_allows_authoritative_duplicates_and_rolls_up_counts()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "counts",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: 3,
                    max_pages: 25,
                },
            )
            .await?;

        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/eligible",
                    preferred_url: Some("https://example.com/eligible"),
                    candidate_state: "eligible",
                    classification_reason: Some("seed_accepted"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/queued",
                    preferred_url: Some("https://example.com/queued"),
                    candidate_state: "queued",
                    classification_reason: Some("seed_accepted"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/processing",
                    preferred_url: Some("https://example.com/processing"),
                    candidate_state: "processing",
                    classification_reason: Some("seed_accepted"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/materialized",
                    preferred_url: Some("https://example.com/materialized"),
                    candidate_state: "materialized",
                    classification_reason: Some("seed_accepted"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/processed",
                    preferred_url: Some("https://example.com/processed"),
                    candidate_state: "processed",
                    classification_reason: Some("seed_accepted"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/failed",
                    preferred_url: Some("https://example.com/failed"),
                    candidate_state: "failed",
                    classification_reason: Some("unsupported_content"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/canceled",
                    preferred_url: Some("https://example.com/canceled"),
                    candidate_state: "canceled",
                    classification_reason: Some("cancel_requested"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/duplicate",
                    preferred_url: Some("https://example.com/duplicate"),
                    candidate_state: "duplicate",
                    classification_reason: Some("duplicate_\u{63}anonical_url"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/excluded",
                    preferred_url: Some("https://example.com/excluded"),
                    candidate_state: "excluded",
                    classification_reason: Some("outside_boundary_policy"),
                },
            )
            .await?;
        fixture
            .create_page(
                &pool,
                run.id,
                DiscoveredPageSeed {
                    normalized_url: "https://example.com/blocked",
                    preferred_url: Some("https://example.com/blocked"),
                    candidate_state: "blocked",
                    classification_reason: Some("inaccessible"),
                },
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
                preferred_url: Some("https://example.com/duplicate"),
                depth: 1,
                referrer_candidate_id: None,
                host_classification: "same_host",
                candidate_state: "duplicate",
                classification_reason: Some("duplicate_\u{63}anonical_url"),
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
        .context("duplicate preferred URL alias should persist")?;

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

        assert_eq!(counts.discovered, 12);
        assert_eq!(counts.eligible, 7);
        assert_eq!(counts.processed, 1);
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.processing, 2);
        assert_eq!(counts.duplicates, 2);
        assert_eq!(counts.excluded, 2);
        assert_eq!(counts.blocked, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.canceled, 1);
        assert!(counts.last_activity_at.is_some());
        assert_eq!(duplicate_alias.preferred_url.as_deref(), Some("https://example.com/duplicate"));
        assert_eq!(duplicate_alias.candidate_state, "duplicate");
        assert_eq!(queued_page.candidate_state, "queued");
        assert_eq!(queued_page.classification_reason.as_deref(), Some("seed_accepted"));

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_ingest_run_repository_persists_cancellation_markers() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "cancel",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: 2,
                    max_pages: 20,
                },
            )
            .await?;
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_page_settlement_waits_for_its_exact_mutation_item_before_completing_run()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for web ingest settlement test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "settlement",
                    mode: "single_page",
                    boundary_policy: "same_host",
                    max_depth: 0,
                    max_pages: 1,
                },
            )
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
        .context("failed to start settlement run")?;
        let (page_id, mutation_item_id) = fixture
            .create_materialized_page(&pool, run.id, run.mutation_id, "pending", "pending")
            .await?;
        let observed = get_web_run_counts(&pool, run.id).await?;
        assert_eq!(observed.discovered, 1, "single-page run must materialize its canonical seed");
        assert_eq!(observed.excluded, 0, "successful seed must not retain a fabricated exclusion");

        let linked_page = ingest_repository::get_web_discovered_page_by_id(&pool, page_id)
            .await?
            .context("linked settlement page disappeared")?;
        ingest_repository::update_web_discovered_page(
            &pool,
            page_id,
            &ingest_repository::UpdateWebDiscoveredPage {
                final_url: linked_page.final_url.as_deref(),
                preferred_url: linked_page.preferred_url.as_deref(),
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
                preferred_url: linked_page.preferred_url.as_deref(),
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
        let pending_mutation =
            content_mutation_repository::get_mutation_by_id(&pool, run.mutation_id)
                .await?
                .context("pending settlement mutation disappeared")?;
        assert_eq!(pending_page.candidate_state, "materialized");
        assert_eq!(pending_run.run_state, "processing");
        assert_eq!(pending_mutation.mutation_state, "accepted");

        let item = content_mutation_repository::get_mutation_item_by_id(&pool, mutation_item_id)
            .await?
            .context("pending settlement item disappeared")?;
        content_mutation_repository::update_mutation_item(
            &pool,
            content_mutation_repository::MutationItemUpdate {
                item_id: item.id,
                document_id: item.document_id,
                base_revision_id: item.base_revision_id,
                result_revision_id: item.result_revision_id,
                item_state: "applied",
                message: None,
            },
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
        let settled_mutation =
            content_mutation_repository::get_mutation_by_id(&pool, run.mutation_id)
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn web_page_settlement_handles_terminal_failure_without_first_item_shortcuts()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for web ingest failure settlement test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = WebIngestRepositoryFixture::create(&pool).await?;

    let result = async {
        let run = fixture
            .create_run(
                &pool,
                WebRunSeed {
                    suffix: "partial-settlement",
                    mode: "recursive_crawl",
                    boundary_policy: "same_host",
                    max_depth: 1,
                    max_pages: 2,
                },
            )
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

        let failed_item =
            content_mutation_repository::get_mutation_item_by_id(&pool, failed_item_id)
                .await?
                .context("failed settlement item disappeared")?;
        content_mutation_repository::update_mutation_item(
            &pool,
            content_mutation_repository::MutationItemUpdate {
                item_id: failed_item.id,
                document_id: failed_item.document_id,
                base_revision_id: failed_item.base_revision_id,
                result_revision_id: failed_item.result_revision_id,
                item_state: "failed",
                message: Some("terminal child failure"),
            },
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
        let terminal_mutation =
            content_mutation_repository::get_mutation_by_id(&pool, run.mutation_id)
                .await?
                .context("partial terminal mutation disappeared")?;
        assert_eq!(terminal_mutation.mutation_state, "applied");

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}
