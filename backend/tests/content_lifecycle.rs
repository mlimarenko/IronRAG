use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        graph_store::{
            GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite,
            GraphProjectionWriteError, GraphStore,
        },
        persistence::Persistence,
        repositories::content_repository,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content_service::{
            AcceptMutationCommand, CreateDocumentCommand, CreateMutationItemCommand,
            CreateRevisionCommand, PromoteHeadCommand, UpdateMutationCommand,
            UpdateMutationItemCommand,
        },
    },
};

struct NoopGraphStore;

#[async_trait]
impl GraphStore for NoopGraphStore {
    fn backend_name(&self) -> &'static str {
        "noop"
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn load_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
    ) -> Result<GraphProjectionData> {
        Ok(GraphProjectionData::default())
    }
}

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

struct ContentLifecycleFixture {
    state: AppState,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl ContentLifecycleFixture {
    async fn create() -> Result<Self> {
        let settings =
            Settings::from_env().context("failed to load settings for content lifecycle test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect content lifecycle postgres")?;

        sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
            .execute(&postgres)
            .await
            .context("failed to apply canonical 0001_init.sql for content lifecycle test")?;

        let state = build_test_state(settings, postgres)?;
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

        Ok(Self { state, temp_database, workspace_id: workspace.id, library_id: library.id })
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for content lifecycle test state")?,
    };
    Ok(AppState::from_dependencies(settings, persistence, Arc::new(NoopGraphStore)))
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

        let by_external_key = content_repository::get_document_by_external_key(
            fixture.pool(),
            fixture.library_id,
            &external_key,
        )
        .await
        .context("failed to load logical document by external key")?
        .context("logical document missing by external key")?;
        assert_eq!(by_external_key.id, document.id);

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

        let loaded_head = fixture
            .state
            .canonical_services
            .content
            .get_document_head(&fixture.state, document.id)
            .await
            .context("failed to load promoted head")?
            .context("missing promoted head")?;
        assert_eq!(loaded_head.active_revision_id, Some(active_revision.id));
        assert_eq!(loaded_head.readable_revision_id, Some(readable_revision.id));

        let summary = fixture
            .state
            .canonical_services
            .content
            .get_document(&fixture.state, document.id)
            .await
            .context("failed to load content document summary")?;
        let summary_head = summary.head.context("missing summary head")?;
        let summary_active_revision = summary.active_revision.context("missing active revision")?;
        assert_eq!(summary.document.document_state, "active");
        assert_eq!(summary_head.active_revision_id, Some(active_revision.id));
        assert_eq!(summary_head.readable_revision_id, Some(readable_revision.id));
        assert_eq!(summary_head.latest_mutation_id, Some(mutation.id));
        assert_eq!(summary_active_revision.id, active_revision.id);
        assert_ne!(summary_head.readable_revision_id, summary_head.active_revision_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_tracks_append_replace_delete_and_mutation_item_states()
-> Result<()> {
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

        let append_mutation = fixture
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
                    idempotency_key: Some(format!("append-{}", Uuid::now_v7())),
                },
            )
            .await
            .context("failed to accept append mutation")?;
        let pending_append_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: append_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(base_revision.id),
                    result_revision_id: None,
                    item_state: "pending".to_string(),
                    message: Some("append scheduled".to_string()),
                },
            )
            .await
            .context("failed to create pending append item")?;
        assert_eq!(pending_append_item.item_state, "pending");

        let appended_revision = fixture
            .state
            .canonical_services
            .content
            .append_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:mutation-append",
                    "Appended Content",
                    None,
                ),
            )
            .await
            .context("failed to append content revision")?;
        let applied_append_item = fixture
            .state
            .canonical_services
            .content
            .update_mutation_item(
                &fixture.state,
                UpdateMutationItemCommand {
                    item_id: pending_append_item.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(base_revision.id),
                    result_revision_id: Some(appended_revision.id),
                    item_state: "applied".to_string(),
                    message: Some("append applied".to_string()),
                },
            )
            .await
            .context("failed to mark append item applied")?;
        assert_eq!(applied_append_item.item_state, "applied");
        assert_eq!(applied_append_item.result_revision_id, Some(appended_revision.id));

        let failed_append_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: append_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(base_revision.id),
                    result_revision_id: None,
                    item_state: "pending".to_string(),
                    message: Some("append retry pending".to_string()),
                },
            )
            .await
            .context("failed to create failed append item placeholder")?;
        let failed_append_item = fixture
            .state
            .canonical_services
            .content
            .update_mutation_item(
                &fixture.state,
                UpdateMutationItemCommand {
                    item_id: failed_append_item.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(base_revision.id),
                    result_revision_id: None,
                    item_state: "failed".to_string(),
                    message: Some("append provider failure".to_string()),
                },
            )
            .await
            .context("failed to mark append item failed")?;
        assert_eq!(failed_append_item.item_state, "failed");

        let append_mutation = fixture
            .state
            .canonical_services
            .content
            .update_mutation(
                &fixture.state,
                UpdateMutationCommand {
                    mutation_id: append_mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await
            .context("failed to mark append mutation applied")?;
        assert_eq!(append_mutation.mutation_state, "applied");

        let replace_mutation = fixture
            .state
            .canonical_services
            .content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "replace".to_string(),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    idempotency_key: Some(format!("replace-{}", Uuid::now_v7())),
                },
            )
            .await
            .context("failed to accept replace mutation")?;
        let replaced_revision = fixture
            .state
            .canonical_services
            .content
            .replace_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "replace",
                    "sha256:mutation-replace",
                    "Replacement Content",
                    Some("file:///replace.txt"),
                ),
            )
            .await
            .context("failed to replace revision content")?;
        let applied_replace_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: replace_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(appended_revision.id),
                    result_revision_id: Some(replaced_revision.id),
                    item_state: "applied".to_string(),
                    message: Some("replace applied".to_string()),
                },
            )
            .await
            .context("failed to create applied replace item")?;
        let conflicted_replace_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: replace_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(appended_revision.id),
                    result_revision_id: None,
                    item_state: "conflicted".to_string(),
                    message: Some("stale base revision".to_string()),
                },
            )
            .await
            .context("failed to create conflicted replace item")?;
        assert_eq!(applied_replace_item.item_state, "applied");
        assert_eq!(conflicted_replace_item.item_state, "conflicted");

        fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(replaced_revision.id),
                    readable_revision_id: Some(replaced_revision.id),
                    latest_mutation_id: Some(replace_mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote replacement head")?;

        let delete_mutation = fixture
            .state
            .canonical_services
            .content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "delete".to_string(),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    idempotency_key: Some(format!("delete-{}", Uuid::now_v7())),
                },
            )
            .await
            .context("failed to accept delete mutation")?;
        let applied_delete_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: delete_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(replaced_revision.id),
                    result_revision_id: None,
                    item_state: "applied".to_string(),
                    message: Some("delete applied".to_string()),
                },
            )
            .await
            .context("failed to create applied delete item")?;
        let skipped_delete_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: delete_mutation.id,
                    document_id: Some(document.id),
                    base_revision_id: Some(replaced_revision.id),
                    result_revision_id: None,
                    item_state: "skipped".to_string(),
                    message: Some("delete skipped because already tombstoned".to_string()),
                },
            )
            .await
            .context("failed to create skipped delete item")?;
        assert_eq!(applied_delete_item.item_state, "applied");
        assert_eq!(skipped_delete_item.item_state, "skipped");

        let deleted_document = fixture
            .state
            .canonical_services
            .content
            .delete_document(&fixture.state, document.id)
            .await
            .context("failed to delete document")?;
        assert_eq!(deleted_document.document_state, "deleted");

        let delete_mutation = fixture
            .state
            .canonical_services
            .content
            .update_mutation(
                &fixture.state,
                UpdateMutationCommand {
                    mutation_id: delete_mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await
            .context("failed to mark delete mutation applied")?;
        assert_eq!(delete_mutation.mutation_state, "applied");

        let delete_items = fixture
            .state
            .canonical_services
            .content
            .list_mutation_items(&fixture.state, delete_mutation.id)
            .await
            .context("failed to list delete mutation items")?;
        let delete_item_states: Vec<&str> =
            delete_items.iter().map(|item| item.item_state.as_str()).collect();
        assert!(delete_item_states.contains(&"applied"));
        assert!(delete_item_states.contains(&"skipped"));

        let persisted_document =
            content_repository::get_document_by_id(fixture.pool(), document.id)
                .await
                .context("failed to reload deleted document")?
                .context("deleted document missing from repository")?;
        assert_eq!(persisted_document.document_state, "deleted");
        assert!(persisted_document.deleted_at.is_some());

        let post_delete_head = fixture
            .state
            .canonical_services
            .content
            .get_document_head(&fixture.state, document.id)
            .await
            .context("failed to load head after delete")?
            .context("missing document head after delete")?;
        assert_eq!(post_delete_head.active_revision_id, None);
        assert_eq!(post_delete_head.readable_revision_id, Some(replaced_revision.id));
        assert_eq!(post_delete_head.latest_mutation_id, Some(replace_mutation.id));

        let post_delete_summary = fixture
            .state
            .canonical_services
            .content
            .get_document(&fixture.state, document.id)
            .await
            .context("failed to load document summary after delete")?;
        assert_eq!(post_delete_summary.document.document_state, "deleted");
        assert!(post_delete_summary.active_revision.is_none());
        assert_eq!(
            post_delete_summary.head.as_ref().and_then(|head| head.readable_revision_id),
            Some(replaced_revision.id)
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
