#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
            client::ArangoClient,
        },
        persistence::Persistence,
        repositories::iam_repository,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::{
            AcceptMutationCommand, AdmitMutationCommand, AppendInlineMutationCommand,
            CreateDocumentCommand, CreateRevisionCommand, PromoteHeadCommand,
            ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
        },
        ingest::web::CreateWebIngestRunCommand,
    },
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("content_lifecycle_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for content lifecycle test")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        Ok(Self {
            name: database_name.clone(),
            admin_url,
            database_url: replace_database_name(base_database_url, &database_name)?,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect admin postgres for content lifecycle cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: reqwest::Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("content_lifecycle_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database for content_lifecycle")?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to create temp ArangoDB database {}: status {}",
                name,
                response.status()
            ));
        }

        Ok(Self {
            base_url,
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
            name,
            http,
        })
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to drop temp ArangoDB database for content_lifecycle")?;
        if response.status() != reqwest::StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct ContentLifecycleFixture {
    state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl ContentLifecycleFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for content lifecycle test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect content lifecycle postgres")?;

        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply canonical 0001_init.sql for content lifecycle test")?;

        let arango_client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
        );
        arango_client.ping().await.context("failed to ping temp ArangoDB for content lifecycle")?;
        bootstrap_knowledge_plane(
            &arango_client,
            &ArangoBootstrapOptions {
                collections: true,
                views: false,
                graph: true,
                vector_indexes: false,
                vector_dimensions: 3072,
                vector_index_n_lists: 100,
                vector_index_default_n_probe: 8,
                vector_index_training_iterations: 25,
            },
        )
        .await
        .context("failed to bootstrap Arango knowledge plane for content lifecycle")?;

        let persistence = Persistence {
            postgres,
            redis: redis::Client::open(settings.redis_url.clone())
                .context("failed to create redis client for content lifecycle test state")?,
        };
        let state = AppState::from_dependencies(settings, persistence, arango_client)?;
        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("content-workspace-{}", Uuid::now_v7().simple())),
                    display_name: "Content Lifecycle Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create content lifecycle workspace")?;
        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("content-library-{}", Uuid::now_v7().simple())),
                    display_name: "Content Lifecycle Library".to_string(),
                    description: Some("canonical content lifecycle test fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create content lifecycle library")?;

        Ok(Self {
            state,
            temp_database,
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
        self.temp_database.drop().await
    }
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
}

fn revision_command(
    document_id: Uuid,
    source_kind: &str,
    checksum: &str,
    title: &str,
    source_uri: Option<&str>,
) -> CreateRevisionCommand {
    CreateRevisionCommand {
        document_id,
        content_source_kind: source_kind.to_string(),
        checksum: checksum.to_string(),
        mime_type: "text/plain".to_string(),
        byte_size: 128,
        title: Some(title.to_string()),
        language_code: Some("en".to_string()),
        source_uri: source_uri.map(ToString::to_string),
        storage_key: Some(format!("storage/{checksum}")),
        created_by_principal_id: None,
    }
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_preserves_logical_document_identity_and_revision_lineage()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let external_key = format!("logical-doc-{}", Uuid::now_v7());
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(external_key.clone()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create canonical content document")?;
        assert_eq!(document.workspace_id, fixture.workspace_id);
        assert_eq!(document.library_id, fixture.library_id);
        assert_eq!(document.external_key, external_key);
        assert_eq!(document.document_state, "active");

        let knowledge_document = fixture
            .state
            .arango_document_store
            .get_document(document.id)
            .await
            .context("failed to load knowledge document shell for content lifecycle")?
            .context("knowledge document shell missing from arango")?;
        assert_eq!(knowledge_document.external_key, external_key);
        assert_eq!(knowledge_document.document_state, "active");

        let first_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:lifecycle-upload",
                    "Initial Upload",
                    Some("file:///initial.txt"),
                ),
            )
            .await
            .context("failed to create initial revision")?;
        let appended_revision = fixture
            .state
            .canonical_services
            .content
            .append_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:lifecycle-append",
                    "Appended Revision",
                    None,
                ),
            )
            .await
            .context("failed to append revision")?;
        let replaced_revision = fixture
            .state
            .canonical_services
            .content
            .replace_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "replace",
                    "sha256:lifecycle-replace",
                    "Replacement Revision",
                    Some("file:///replacement.txt"),
                ),
            )
            .await
            .context("failed to replace revision")?;

        assert_eq!(first_revision.revision_number, 1);
        assert_eq!(appended_revision.revision_number, 2);
        assert_eq!(replaced_revision.revision_number, 3);
        assert_eq!(appended_revision.parent_revision_id, Some(first_revision.id));
        assert_eq!(replaced_revision.parent_revision_id, Some(appended_revision.id));
        assert_eq!(appended_revision.document_id, document.id);
        assert_eq!(replaced_revision.document_id, document.id);

        let revisions = fixture
            .state
            .canonical_services
            .content
            .list_revisions(&fixture.state, document.id)
            .await
            .context("failed to list canonical revisions")?;
        assert_eq!(revisions.len(), 3);
        assert_eq!(
            revisions.iter().map(|revision| revision.id).collect::<Vec<_>>(),
            vec![replaced_revision.id, appended_revision.id, first_revision.id]
        );

        let knowledge_revisions = fixture
            .state
            .arango_document_store
            .list_revisions_by_document(document.id)
            .await
            .context("failed to list knowledge revisions for content lifecycle")?;
        assert_eq!(
            knowledge_revisions.iter().map(|revision| revision.revision_id).collect::<Vec<_>>(),
            vec![replaced_revision.id, appended_revision.id, first_revision.id]
        );
        assert_eq!(knowledge_revisions[0].revision_kind, "replace");
        assert_eq!(knowledge_revisions[1].revision_kind, "append");
        assert_eq!(knowledge_revisions[2].revision_kind, "upload");

        let summaries = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list canonical document summaries")?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].document.id, document.id);
        assert_eq!(summaries[0].document.external_key, external_key);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_promotes_head_and_separates_readable_from_active() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("head-doc-{}", Uuid::now_v7())),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create head lifecycle document")?;
        let readable_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:head-readable",
                    "Readable Revision",
                    Some("file:///readable.txt"),
                ),
            )
            .await
            .context("failed to create readable revision")?;
        let active_revision = fixture
            .state
            .canonical_services
            .content
            .append_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:head-active",
                    "Active Revision",
                    None,
                ),
            )
            .await
            .context("failed to create active revision")?;
        let mutation = fixture
            .state
            .canonical_services
            .content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "append".to_string(),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    idempotency_key: None,
                    source_identity: None,
                },
            )
            .await
            .context("failed to accept append mutation")?;

        let promoted_head = fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(active_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote document head")?;
        assert_eq!(promoted_head.active_revision_id, Some(active_revision.id));
        assert_eq!(promoted_head.readable_revision_id, Some(readable_revision.id));
        assert_eq!(promoted_head.latest_mutation_id, Some(mutation.id));

        let knowledge_document = fixture
            .state
            .arango_document_store
            .get_document(document.id)
            .await
            .context("failed to load promoted knowledge document")?
            .context("missing promoted knowledge document")?;
        assert_eq!(knowledge_document.document_state, "active");
        assert_eq!(knowledge_document.active_revision_id, Some(active_revision.id));
        assert_eq!(knowledge_document.readable_revision_id, Some(readable_revision.id));
        assert_ne!(knowledge_document.readable_revision_id, knowledge_document.active_revision_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_inline_upload_admits_background_ingest_job() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let admission = fixture
            .state
            .canonical_services
            .content
            .upload_inline_document(
                &fixture.state,
                UploadInlineDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("inline-upload-{}", Uuid::now_v7())),
                    idempotency_key: None,
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    source_identity: Some("content-lifecycle-inline-upload".to_string()),
                    file_name: "inline-upload.txt".to_string(),
                    title: Some("Inline Upload".to_string()),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes: b"Ada Lovelace wrote the note.\nCharles Babbage built the engine."
                        .to_vec(),
                },
            )
            .await
            .context("failed to upload inline content document")?;
        let revision_id = admission
            .mutation
            .items
            .first()
            .and_then(|item| item.result_revision_id)
            .context("inline upload did not create a result revision")?;
        let revision = fixture
            .state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .context("failed to load admitted inline upload revision")?
            .context("missing admitted inline upload revision")?;

        let postgres_chunks =
            rustrag_backend::infra::repositories::content_repository::list_chunks_by_revision(
                &fixture.state.persistence.postgres,
                revision_id,
            )
            .await
            .context("failed to list postgres chunks for inline upload")?;
        let knowledge_chunks = fixture
            .state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .context("failed to list Arango knowledge chunks for inline upload")?;
        let ingest_jobs = rustrag_backend::infra::repositories::ingest_repository::list_ingest_jobs_by_mutation_ids(
            &fixture.state.persistence.postgres,
            fixture.workspace_id,
            fixture.library_id,
            &[admission.mutation.mutation.id],
        )
        .await
        .context("failed to list ingest jobs for inline upload")?;

        assert_eq!(admission.mutation.mutation.mutation_state, "accepted");
        assert!(revision.storage_ref.is_some());
        assert!(postgres_chunks.is_empty());
        assert!(knowledge_chunks.is_empty());
        assert_eq!(ingest_jobs.len(), 1);
        assert_eq!(ingest_jobs[0].mutation_id, Some(admission.mutation.mutation.id));
        assert_eq!(ingest_jobs[0].queue_state, "queued");
        assert_eq!(ingest_jobs[0].job_kind, "content_mutation");

        let summaries = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list canonical document summaries after inline upload")?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].document.id, admission.document.document.id);
        assert_eq!(
            summaries[0]
                .pipeline
                .latest_mutation
                .as_ref()
                .map(|mutation| mutation.id),
            Some(admission.mutation.mutation.id)
        );
        assert_eq!(
            summaries[0]
                .pipeline
                .latest_job
                .as_ref()
                .map(|job| job.id),
            Some(ingest_jobs[0].id)
        );
        assert_eq!(
            summaries[0]
                .pipeline
                .latest_job
                .as_ref()
                .map(|job| job.queue_state.as_str()),
            Some("queued")
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_single_page_web_ingest_materializes_only_the_seed_page()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let seed_url = server.url("/seed");
        let run = fixture
            .state
            .canonical_services
            .web_ingest
            .create_run(
                &fixture.state,
                CreateWebIngestRunCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    seed_url: seed_url.clone(),
                    mode: "single_page".to_string(),
                    boundary_policy: None,
                    max_depth: None,
                    max_pages: None,
                    requested_by_principal_id: None,
                    request_surface: "test".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit single-page web ingest run")?;

        assert_eq!(run.mode, "single_page");
        assert_eq!(run.run_state, "completed");

        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list single-page web ingest pages")?;
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, seed_url);
        assert_eq!(pages[0].candidate_state, "processed");
        assert!(pages[0].document_id.is_some());
        assert!(pages[0].result_revision_id.is_some());

        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after single-page web ingest")?;
        assert_eq!(documents.len(), 1);

        let summary = &documents[0];
        assert_eq!(summary.document.external_key, server.url("/seed"));
        assert_eq!(
            summary.active_revision.as_ref().and_then(|revision| revision.source_uri.as_deref()),
            Some(server.url("/seed").as_str())
        );
        assert_eq!(
            summary.active_revision.as_ref().map(|revision| revision.content_source_kind.as_str()),
            Some("web_page")
        );
        assert_eq!(
            summary.web_page_provenance.as_ref().and_then(|value| value.run_id),
            Some(run.run_id)
        );
        assert_eq!(
            summary.web_page_provenance.as_ref().and_then(|value| value.candidate_id),
            Some(pages[0].candidate_id)
        );

        let revisions = fixture
            .state
            .canonical_services
            .content
            .list_revisions(&fixture.state, summary.document.id)
            .await
            .context("failed to list revisions after single-page web ingest")?;
        assert_eq!(revisions.len(), 1);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_tracks_append_replace_delete_and_mutation_item_states()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Content Lifecycle Mutation Principal",
            None,
        )
        .await
        .context("failed to create content lifecycle mutation principal")?;
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("mutation-doc-{}", Uuid::now_v7())),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create mutation lifecycle document")?;
        let base_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:mutation-base",
                    "Base Revision",
                    Some("file:///base.txt"),
                ),
            )
            .await
            .context("failed to create base revision")?;
        fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(base_revision.id),
                    readable_revision_id: Some(base_revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote base head")?;

        let replace_admission = fixture
            .state
            .canonical_services
            .content
            .replace_inline_mutation(
                &fixture.state,
                ReplaceInlineMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some("canonical-replace".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    file_name: "replacement.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes:
                        b"Replacement content that must stay pending until ingest finishes."
                            .to_vec(),
                },
            )
            .await
            .context("failed to admit canonical replace mutation")?;
        assert_eq!(replace_admission.mutation.mutation_state, "accepted");
        let replace_mutation_id = replace_admission.mutation.id;
        let replace_item = replace_admission
            .items
            .first()
            .context("replace admission must create one mutation item")?;
        assert_eq!(replace_item.item_state, "pending");
        let replace_revision_id = replace_item
            .result_revision_id
            .context("replace admission must create a pending replacement revision")?;

        let replace_job_handle = fixture
            .state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(&fixture.state, replace_mutation_id)
            .await
            .context("failed to load replace ingest job handle")?
            .context("replace mutation must enqueue an ingest job")?;
        assert_eq!(replace_job_handle.job.queue_state, "queued");
        assert_eq!(replace_job_handle.job.knowledge_revision_id, Some(replace_revision_id));

        let repeated_replace_admission = fixture
            .state
            .canonical_services
            .content
            .replace_inline_mutation(
                &fixture.state,
                ReplaceInlineMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some("canonical-replace".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    file_name: "replacement.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes:
                        b"Replacement content that must stay pending until ingest finishes."
                            .to_vec(),
                },
            )
            .await
            .context("failed to replay canonical replace mutation")?;
        assert_eq!(repeated_replace_admission.mutation.id, replace_mutation_id);
        assert_eq!(
            repeated_replace_admission.items.first().and_then(|item| item.result_revision_id),
            Some(replace_revision_id)
        );
        assert_eq!(repeated_replace_admission.job_id, replace_admission.job_id);
        assert_eq!(
            repeated_replace_admission.async_operation_id,
            replace_admission.async_operation_id
        );

        let head_after_replace = fixture
            .state
            .canonical_services
            .content
            .get_document_head(&fixture.state, document.id)
            .await
            .context("failed to load head after replace admission")?
            .context("document head missing after replace admission")?;
        assert_eq!(head_after_replace.latest_mutation_id, Some(replace_mutation_id));
        assert_eq!(head_after_replace.active_revision_id, Some(base_revision.id));
        assert_eq!(head_after_replace.readable_revision_id, Some(base_revision.id));

        let active_documents_before_delete = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list active documents after replace admission")?;
        assert_eq!(active_documents_before_delete.len(), 1);
        assert_eq!(active_documents_before_delete[0].document.id, document.id);
        assert_eq!(
            active_documents_before_delete[0].active_revision.as_ref().map(|revision| revision.id),
            Some(base_revision.id)
        );
        assert_eq!(
            active_documents_before_delete[0]
                .active_revision
                .as_ref()
                .and_then(|revision| revision.source_uri.as_deref()),
            Some("file:///base.txt")
        );

        let delete_admission = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some("canonical-delete".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                },
            )
            .await
            .context("failed to admit canonical delete mutation")?;
        assert_eq!(delete_admission.mutation.mutation_state, "applied");
        let delete_mutation_id = delete_admission.mutation.id;
        let delete_item = delete_admission
            .items
            .first()
            .context("delete admission must create one mutation item")?;
        assert_eq!(delete_item.item_state, "applied");
        assert_eq!(delete_item.base_revision_id, Some(base_revision.id));
        assert!(delete_admission.job_id.is_none());
        let delete_operation_id = delete_admission
            .async_operation_id
            .context("delete admission must expose a completed async operation")?;
        let delete_operation = fixture
            .state
            .canonical_services
            .ops
            .get_async_operation(&fixture.state, delete_operation_id)
            .await
            .context("failed to reload delete async operation")?;
        assert_eq!(delete_operation.status, "ready");

        let head_after_delete = fixture
            .state
            .canonical_services
            .content
            .get_document_head(&fixture.state, document.id)
            .await
            .context("failed to load head after delete")?
            .context("document head missing after delete")?;
        assert_eq!(head_after_delete.active_revision_id, None);
        assert_eq!(head_after_delete.readable_revision_id, Some(base_revision.id));
        assert_eq!(head_after_delete.latest_mutation_id, Some(delete_mutation_id));

        let replace_admission_after_delete = fixture
            .state
            .canonical_services
            .content
            .get_mutation_admission(&fixture.state, replace_mutation_id)
            .await
            .context("failed to reload superseded replace mutation after delete")?;
        assert_eq!(replace_admission_after_delete.mutation.mutation_state, "canceled");
        assert_eq!(
            replace_admission_after_delete.mutation.failure_code.as_deref(),
            Some("document_deleted")
        );
        assert!(
            replace_admission_after_delete.items.iter().all(|item| item.item_state == "skipped"),
            "delete must settle all superseded replace items as skipped"
        );
        let replace_async_operation_id = replace_admission_after_delete
            .async_operation_id
            .context("replace admission must retain its async operation id")?;
        let replace_async_operation = fixture
            .state
            .canonical_services
            .ops
            .get_async_operation(&fixture.state, replace_async_operation_id)
            .await
            .context("failed to reload superseded replace async operation")?;
        assert_eq!(replace_async_operation.status, "failed");
        assert_eq!(replace_async_operation.failure_code.as_deref(), Some("document_deleted"));

        let knowledge_document = fixture
            .state
            .arango_document_store
            .get_document(document.id)
            .await
            .context("failed to reload deleted knowledge document")?
            .context("deleted knowledge document missing from arango")?;
        assert_eq!(knowledge_document.document_state, "deleted");
        assert_eq!(knowledge_document.active_revision_id, None);
        assert_eq!(knowledge_document.readable_revision_id, Some(base_revision.id));
        assert!(knowledge_document.deleted_at.is_some());

        let active_documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list active documents after delete")?;
        assert!(
            active_documents.iter().all(|summary| summary.document.id != document.id),
            "deleted document must not appear in canonical active document listings"
        );

        let all_documents = fixture
            .state
            .canonical_services
            .content
            .list_documents_with_deleted(&fixture.state, fixture.library_id, true)
            .await
            .context("failed to list documents including deleted after delete")?;
        assert!(
            all_documents.iter().any(|summary| {
                summary.document.id == document.id && summary.document.document_state == "deleted"
            }),
            "explicit include_deleted listing must retain deleted documents"
        );
        let ops_snapshot = fixture
            .state
            .canonical_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to refresh ops snapshot after delete")?;
        assert_eq!(ops_snapshot.state.readable_document_count, 0);
        assert_eq!(ops_snapshot.state.failed_document_count, 0);

        let knowledge_summary = fixture
            .state
            .canonical_services
            .knowledge
            .get_library_summary(&fixture.state, fixture.library_id)
            .await
            .context("failed to refresh knowledge summary after delete")?;
        assert!(
            knowledge_summary.document_counts_by_readiness.is_empty(),
            "deleted documents must not contribute to knowledge summary readiness counts"
        );

        let repeated_delete = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: None,
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                },
            )
            .await;
        let repeated_delete =
            repeated_delete.context("failed to replay canonical delete mutation")?;
        assert_eq!(repeated_delete.mutation.id, delete_mutation_id);
        assert_eq!(repeated_delete.mutation.mutation_state, "applied");

        let library_mutations = fixture
            .state
            .canonical_services
            .content
            .list_mutations(&fixture.state, fixture.library_id)
            .await
            .context("failed to list library mutations after repeated delete")?;
        assert_eq!(
            library_mutations.iter().filter(|mutation| mutation.operation_kind == "delete").count(),
            1,
            "repeated delete must reuse the canonical delete mutation"
        );

        let append_after_delete = fixture
            .state
            .canonical_services
            .content
            .append_inline_mutation(
                &fixture.state,
                AppendInlineMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some(format!("append-after-delete-{}", Uuid::now_v7())),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "test".to_string(),
                    source_identity: None,
                    appended_text: "this must be rejected".to_string(),
                },
            )
            .await;
        assert!(
            append_after_delete.is_err(),
            "deleted documents must reject subsequent append mutations"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
