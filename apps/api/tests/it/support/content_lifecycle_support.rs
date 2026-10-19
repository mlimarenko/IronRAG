use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::persistence::Persistence,
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::CreateRevisionCommand,
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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

pub(crate) struct ContentLifecycleFixture {
    pub(crate) state: AppState,
    temp_database: TempDatabase,
    _content_storage_root: tempfile::TempDir,
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
}

/// Imports a fixture descriptor before its revision becomes immutable.
pub(crate) async fn create_imported_revision(
    state: &AppState,
    command: CreateRevisionCommand,
    file_name: &str,
) -> Result<ironrag_backend::domains::content::ContentRevision> {
    use ironrag_backend::infra::repositories::{
        content_document_repository, content_revision_repository,
    };
    let document = content_document_repository::get_document_by_id(
        &state.persistence.postgres,
        command.document_id,
    )
    .await?
    .context("fixture source document")?;
    let admission = crate::content_repository::synthetic_import_admission(
        ironrag_contracts::documents::ReplacementIdentity {
            checksum: command.checksum.clone(),
            byte_size: command.byte_size,
            mime_type: command.mime_type.clone(),
            file_name: file_name.to_owned(),
            title: command.title.clone(),
            language_code: command.language_code.clone(),
            source_uri: command.source_uri.clone(),
            document_hint: command.document_hint.clone(),
        },
    )?;
    let outcome = content_revision_repository::create_revision_with_projection(
        &state.persistence.postgres,
        &content_revision_repository::NewContentRevisionProjection {
            admission_identity: Some(&admission),
            document_id: document.id,
            workspace_id: document.workspace_id,
            library_id: document.library_id,
            content_source_kind: &command.content_source_kind,
            checksum: &command.checksum,
            mime_type: &command.mime_type,
            byte_size: command.byte_size,
            title: command.title.as_deref(),
            language_code: command.language_code.as_deref(),
            source_uri: command.source_uri.as_deref(),
            document_hint: command.document_hint.as_deref(),
            storage_key: command.storage_key.as_deref(),
            created_by_principal_id: command.created_by_principal_id,
        },
    )
    .await?;
    let content_revision_repository::CreateContentRevisionOutcome::Created(row) = outcome else {
        anyhow::bail!("fixture import could not create revision: {outcome:?}");
    };
    Ok(ironrag_backend::domains::content::ContentRevision {
        id: row.id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: row.revision_number,
        parent_revision_id: row.parent_revision_id,
        content_source_kind: row.content_source_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: row.language_code,
        source_uri: row.source_uri,
        document_hint: row.document_hint,
        storage_key: row.storage_key,
        created_by_principal_id: row.created_by_principal_id,
        created_at: row.created_at,
    })
}

/// Finds existing publication authority; imported fixtures never acquire fake mutations.
pub(crate) async fn publication_item(
    postgres: &PgPool,
    revision_id: Uuid,
    file_name: &str,
) -> Result<Option<Uuid>> {
    use ironrag_contracts::documents::RevisionAdmissionIdentity;
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("select admission_identity from content_revision where id=$1")
            .bind(revision_id)
            .fetch_one(postgres)
            .await?;
    let admission: RevisionAdmissionIdentity =
        serde_json::from_value(value.context("fixture revision must be admitted at insertion")?)?;
    anyhow::ensure!(
        admission.replacement().file_name == file_name,
        "fixture publication filename differs from initial admission"
    );
    if matches!(admission, RevisionAdmissionIdentity::CertifiedImport { .. }) {
        return Ok(None);
    }
    let items: Vec<Uuid> = sqlx::query_scalar("select i.id from content_mutation_item i join content_mutation m on m.id=i.mutation_id join content_revision r on r.id=i.result_revision_id where r.id=$1 and i.item_state='applied' and r.admission_identity->'replacement'=m.request_identity->'replacement' and r.admission_identity->'sourceIdentity'=m.request_identity->'sourceIdentity' and r.admission_identity->'materializationToken'=m.request_identity->'materializationToken' and r.admission_identity->'visibilityIntentId'=coalesce(to_jsonb(i.visibility_intent_id),'null'::jsonb)")
        .bind(revision_id).fetch_all(postgres).await?;
    anyhow::ensure!(
        items.len() == 1,
        "fixture requires exactly one existing applied publication item"
    );
    Ok(Some(items[0]))
}

pub(crate) async fn seed_pending_revision_owner(
    postgres: &PgPool,
    revision_id: Uuid,
    file_name: &str,
) -> Result<Uuid> {
    use ironrag_backend::infra::repositories::content_mutation_repository::{
        NewContentMutation, NewContentMutationItem, create_mutation, create_mutation_item,
    };
    use ironrag_backend::infra::repositories::content_revision_repository;
    anyhow::ensure!(!file_name.trim().is_empty(), "fixture source filename is required");
    let revision = content_revision_repository::get_revision_by_id(postgres, revision_id)
        .await?
        .context("fixture revision missing")?;
    let identity = serde_json::json!({
        "workspaceId":revision.workspace_id,"libraryId":revision.library_id,"operationKind":"replace",
        "target":{"kind":"existing","documentId":revision.document_id},
        "sourceIdentity":null,"materializationToken":null,
        "revision":{"contentSourceKind":revision.content_source_kind,"baseRevisionId":revision.parent_revision_id,"visibilityIntentId":null},
        "replacement":ironrag_contracts::documents::ReplacementIdentity {
            checksum:revision.checksum,mime_type:revision.mime_type,byte_size:revision.byte_size,
            file_name:file_name.to_owned(),title:revision.title,language_code:revision.language_code,
            source_uri:revision.source_uri,document_hint:revision.document_hint,
        },
    });
    let existing:Vec<Uuid>=sqlx::query_scalar("select i.id from content_mutation_item i join content_mutation m on m.id=i.mutation_id where i.result_revision_id=$1 and i.document_id=$2 and i.item_state::text=$4 and m.request_identity=$3")
        .bind(revision_id).bind(revision.document_id).bind(&identity).bind("pending").fetch_all(postgres).await?;
    if let [owner] = existing.as_slice() {
        return Ok(*owner);
    }
    anyhow::ensure!(existing.is_empty(), "fixture revision has ambiguous exact owners");
    let mutation = create_mutation(
        postgres,
        &NewContentMutation {
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            operation_kind: "replace",
            requested_by_principal_id: None,
            request_surface: "internal",
            idempotency_key: None,
            source_identity: None,
            mutation_state: "running",
            failure_code: None,
            conflict_code: None,
            request_identity: &identity,
        },
    )
    .await?;
    let item = create_mutation_item(
        postgres,
        &NewContentMutationItem {
            mutation_id: mutation.id,
            document_id: revision.document_id,
            base_revision_id: revision.parent_revision_id,
            result_revision_id: Some(revision_id),
            item_state: "pending",
            message: None,
        },
    )
    .await?;
    Ok(item.id)
}

pub(crate) struct PendingContentMutationSeed<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) operation_kind: &'a str,
    pub(crate) request_surface: &'a str,
    pub(crate) mutation_state: &'a str,
    pub(crate) request_identity: &'a serde_json::Value,
}

/// Seeds an unpublished request through the production registry reservation path.
pub(crate) async fn seed_pending_content_mutation(
    postgres: &PgPool,
    seed: PendingContentMutationSeed<'_>,
) -> Result<ironrag_backend::infra::repositories::content_mutation_repository::ContentMutationRow> {
    use ironrag_backend::infra::repositories::content_mutation_repository::{
        NewContentMutation, create_mutation,
    };
    anyhow::ensure!(
        matches!(seed.mutation_state, "accepted" | "running"),
        "pending fixture mutation must be accepted or running"
    );
    Ok(create_mutation(
        postgres,
        &NewContentMutation {
            request_identity: seed.request_identity,
            workspace_id: seed.workspace_id,
            library_id: seed.library_id,
            operation_kind: seed.operation_kind,
            requested_by_principal_id: None,
            request_surface: seed.request_surface,
            idempotency_key: None,
            source_identity: None,
            mutation_state: seed.mutation_state,
            failure_code: None,
            conflict_code: None,
        },
    )
    .await?)
}

pub(crate) async fn seed_readable_revision_head(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
    file_name: &str,
) -> Result<()> {
    use ironrag_backend::infra::repositories::content_document_head_repository::{
        NewContentDocumentHead, upsert_document_head,
    };
    let owner = publication_item(postgres, revision_id, file_name).await?;
    upsert_document_head(
        postgres,
        &NewContentDocumentHead {
            document_id,
            publishing_mutation_item_id: owner,
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    Ok(())
}

/// Sets fixture publication from extraction, never from terminal pipeline status.
pub(crate) async fn seed_extracted_revision_head(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
    file_name: &str,
    extracted_text: Option<&str>,
) -> Result<()> {
    if extracted_text.is_some() {
        seed_readable_revision_head(postgres, document_id, revision_id, file_name).await
    } else {
        seed_unpublished_revision_head(postgres, document_id, revision_id).await
    }
}

/// Establishes private work without pretending an unpublished source is readable.
async fn seed_unpublished_revision_head(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    use ironrag_backend::infra::repositories::content_document_head_repository::{
        NewContentDocumentHead, upsert_document_head,
    };
    upsert_document_head(
        postgres,
        &NewContentDocumentHead {
            document_id,
            active_revision_id: Some(revision_id),
            readable_revision_id: None,
            publishing_mutation_item_id: None,
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    Ok(())
}

/// Projects operational state and proves publication only when the fixture owns one.
pub(crate) async fn promote_fixture_document(
    state: &AppState,
    document_id: Uuid,
    published_revision_id: Option<Uuid>,
) -> Result<()> {
    state
        .application_services
        .knowledge
        .promote_document(
            state,
            ironrag_backend::services::knowledge::service::PromoteKnowledgeDocumentCommand {
                document_id,
            },
        )
        .await?;
    if let Some(revision_id) = published_revision_id {
        assert_current_revision(&state.persistence.postgres, document_id, revision_id).await?;
    }
    Ok(())
}

/// A published fixture must be admitted by the same relation as public reads.
pub(crate) async fn assert_current_revision(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    let current: Vec<Uuid> = sqlx::query_scalar(
        "select revision_id from content_current_revision where document_id = $1",
    )
    .bind(document_id)
    .fetch_all(postgres)
    .await?;
    anyhow::ensure!(
        current == vec![revision_id],
        "fixture publication is not canonically readable"
    );
    Ok(())
}

/// Seeds the text-readable projection of an existing immutable source revision.
pub(crate) async fn seed_readable_revision_projection(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
    file_name: &str,
) -> Result<()> {
    seed_readable_revision_head(postgres, document_id, revision_id, file_name).await?;
    ironrag_backend::infra::repositories::content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
        postgres, document_id,
    ).await?;
    sqlx::query(
        "insert into knowledge_revision (
            revision_id, document_id, workspace_id, library_id, revision_number,
            revision_state, revision_kind, checksum, mime_type, byte_size, title,
            text_state, text_readable_at, vector_state, graph_state, created_at
         ) select id, document_id, workspace_id, library_id, revision_number,
            'ready', content_source_kind::text, checksum, mime_type, byte_size, title,
            'text_readable', now(), 'accepted', 'graph_degraded', created_at
         from content_revision where id = $1",
    )
    .bind(revision_id)
    .execute(postgres)
    .await?;
    assert_current_revision(postgres, document_id, revision_id).await
}

/// Publishes the source of a graph-only fixture, rather than an ownerless shell.
pub(crate) async fn seed_graph_document_revision(
    postgres: &PgPool,
    document_id: Uuid,
    file_name: &str,
    title: &str,
) -> Result<Uuid> {
    let revision_id = Uuid::now_v7();
    use sha2::{Digest, Sha256};
    let checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"")));
    let admission = crate::content_repository::synthetic_import_admission(
        ironrag_contracts::documents::ReplacementIdentity {
            checksum: checksum.clone(),
            byte_size: 0,
            mime_type: "text/plain".to_owned(),
            file_name: file_name.to_owned(),
            title: Some(title.to_owned()),
            language_code: None,
            source_uri: None,
            document_hint: None,
        },
    )?;
    let inserted = sqlx::query(
        "insert into content_revision (
            id, document_id, workspace_id, library_id, revision_number,
            content_source_kind, checksum, mime_type, byte_size, title, admission_identity
         ) select $1, id, workspace_id, library_id, 1,
            'upload', $2, 'text/plain', 0, $3, $5 from content_document where id = $4",
    )
    .bind(revision_id)
    .bind(checksum)
    .bind(title)
    .bind(document_id)
    .bind(serde_json::to_value(admission)?)
    .execute(postgres)
    .await?;
    anyhow::ensure!(inserted.rows_affected() == 1, "graph fixture source document missing");
    seed_readable_revision_projection(postgres, document_id, revision_id, file_name).await?;
    Ok(revision_id)
}

impl ContentLifecycleFixture {
    pub(crate) async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for content lifecycle test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        // Lifecycle tests must never inherit an operator's S3 endpoint or
        // shared storage prefix from `.env`. A unique filesystem backend makes
        // upload/edit/reprocess cases hermetic and lets TempDir remove every
        // source object after the fixture is dropped.
        let content_storage_root =
            tempfile::tempdir().context("failed to create content lifecycle storage root")?;
        settings.dependency_object_storage_mode = "disabled".to_string();
        settings.content_storage_provider = "filesystem".to_string();
        settings.content_storage_topology = "single_node".to_string();
        settings.content_storage_key_prefix.clear();
        settings.content_storage_root = content_storage_root.path().to_string_lossy().into_owned();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect content lifecycle postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply authoritative migrations for content lifecycle test")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for content lifecycle test state")?;
        let persistence = Persistence::for_tests(postgres, redis);
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state = AppState::from_dependencies(settings, persistence)?;
        let workspace = state
            .application_services
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
            .application_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("content-library-{}", Uuid::now_v7().simple())),
                    display_name: "Content Lifecycle Library".to_string(),
                    description: Some("authoritative content lifecycle test fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create content lifecycle library")?;

        Ok(Self {
            state,
            temp_database,
            _content_storage_root: content_storage_root,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    pub(crate) async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
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

pub(crate) fn fixture_checksum(label: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(label.as_bytes())))
}

#[allow(
    dead_code,
    reason = "shared lifecycle fixture helper is not used by every integration-test binary"
)]
pub(crate) fn revision_command(
    document_id: Uuid,
    source_kind: &str,
    checksum: &str,
    title: &str,
    source_uri: Option<&str>,
) -> CreateRevisionCommand {
    // Most lifecycle fixtures name descriptors rather than persist source bytes.
    // Hash those fixture labels; already content-addressed inputs stay unchanged.
    let checksum = if checksum.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        checksum.to_owned()
    } else {
        fixture_checksum(checksum)
    };
    CreateRevisionCommand {
        document_id,
        content_source_kind: source_kind.to_string(),
        checksum: checksum.clone(),
        mime_type: "text/plain".to_string(),
        byte_size: 128,
        title: Some(title.to_string()),
        language_code: Some("en".to_string()),
        source_uri: source_uri.map(ToString::to_string),
        document_hint: None,
        storage_key: Some(format!("storage/{checksum}")),
        created_by_principal_id: None,
    }
}
