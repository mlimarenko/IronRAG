use anyhow::Context;
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    infra::repositories::{
        content_repository,
        content_repository::{
            NewContentChunk, NewContentDocument, NewContentDocumentHead, NewContentMutation,
            NewContentMutationItem, NewContentRevision,
        },
        iam_repository,
    },
};

struct ContentRepositoryFixture {
    principal_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl ContentRepositoryFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let principal = iam_repository::create_principal(pool, "user", "Content Repo Test", None)
            .await
            .context("failed to create content repository principal")?;
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
        .bind(format!("content-repo-{suffix}"))
        .bind("Content Repository Test Workspace")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert content repository workspace")?;
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
        .bind(format!("content-library-{suffix}"))
        .bind("Content Repository Test Library")
        .bind("canonical content repository tests")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert content repository library")?;

        Ok(Self { principal_id: principal.id, workspace_id, library_id })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from catalog_workspace where id = $1")
            .bind(self.workspace_id)
            .execute(pool)
            .await
            .context("failed to delete content repository workspace")?;
        sqlx::query("delete from iam_principal where id = $1")
            .bind(self.principal_id)
            .execute(pool)
            .await
            .context("failed to delete content repository principal")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect content repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for content repository test")?;
    Ok(pool)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn content_repository_persists_logical_document_revision_head_and_chunks()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let external_key = format!("doc-{}", Uuid::now_v7());
        let document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &external_key,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create logical document")?;
        assert_eq!(document.workspace_id, fixture.workspace_id);
        assert_eq!(document.library_id, fixture.library_id);
        assert_eq!(document.external_key, external_key);
        assert_eq!(document.document_state, "active");

        let by_id = content_repository::get_document_by_id(&pool, document.id)
            .await
            .context("failed to load document by id")?
            .context("missing document by id")?;
        let listed = content_repository::list_documents_by_library(&pool, fixture.library_id)
            .await
            .context("failed to list documents by library")?;
        assert_eq!(by_id.id, document.id);
        assert!(listed.iter().any(|row| row.id == document.id));

        let first_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: "sha256:rev-1",
                mime_type: "text/plain",
                byte_size: 128,
                title: Some("Revision One"),
                language_code: Some("en"),
                source_uri: Some("file:///doc-1.txt"),
                storage_key: Some("storage/doc-1"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create first revision")?;
        let second_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 2,
                parent_revision_id: Some(first_revision.id),
                content_source_kind: "append",
                checksum: "sha256:rev-2",
                mime_type: "text/plain",
                byte_size: 192,
                title: Some("Revision Two"),
                language_code: Some("en"),
                source_uri: None,
                storage_key: Some("storage/doc-2"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create second revision")?;

        let latest_revision =
            content_repository::get_latest_revision_for_document(&pool, document.id)
                .await
                .context("failed to load latest revision")?
                .context("missing latest revision")?;
        let revisions = content_repository::list_revisions_by_document(&pool, document.id)
            .await
            .context("failed to list revisions by document")?;
        assert_eq!(latest_revision.id, second_revision.id);
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].revision_number, 2);
        assert_eq!(revisions[1].revision_number, 1);

        let initial_head = content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(first_revision.id),
                readable_revision_id: Some(first_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to create initial document head")?;
        assert_eq!(initial_head.active_revision_id, Some(first_revision.id));

        let updated_head = content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(second_revision.id),
                readable_revision_id: Some(first_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to update document head")?;
        let loaded_head = content_repository::get_document_head(&pool, document.id)
            .await
            .context("failed to load document head")?
            .context("missing document head")?;
        assert_eq!(updated_head.active_revision_id, Some(second_revision.id));
        assert_eq!(loaded_head.readable_revision_id, Some(first_revision.id));

        let first_chunk = content_repository::create_chunk(
            &pool,
            &NewContentChunk {
                revision_id: second_revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 12,
                token_count: Some(3),
                normalized_text: "hello world.",
                text_checksum: "sha256:chunk-1",
            },
        )
        .await
        .context("failed to create first content chunk")?;
        let second_chunk = content_repository::create_chunk(
            &pool,
            &NewContentChunk {
                revision_id: second_revision.id,
                chunk_index: 1,
                start_offset: 12,
                end_offset: 27,
                token_count: Some(4),
                normalized_text: "second segment.",
                text_checksum: "sha256:chunk-2",
            },
        )
        .await
        .context("failed to create second content chunk")?;

        let chunk_by_id = content_repository::get_chunk_by_id(&pool, first_chunk.id)
            .await
            .context("failed to get chunk by id")?
            .context("missing content chunk")?;
        let listed_chunks = content_repository::list_chunks_by_revision(&pool, second_revision.id)
            .await
            .context("failed to list chunks by revision")?;
        assert_eq!(chunk_by_id.chunk_index, 0);
        assert_eq!(listed_chunks.len(), 2);
        assert_eq!(listed_chunks[0].id, first_chunk.id);
        assert_eq!(listed_chunks[1].id, second_chunk.id);

        let deleted = content_repository::delete_chunks_by_revision(&pool, second_revision.id)
            .await
            .context("failed to delete chunks by revision")?;
        let remaining_chunks =
            content_repository::list_chunks_by_revision(&pool, second_revision.id)
                .await
                .context("failed to re-list chunks after delete")?;
        assert_eq!(deleted, 2);
        assert!(remaining_chunks.is_empty());

        let deleted_document = content_repository::update_document_state(
            &pool,
            document.id,
            "deleted",
            Some(Utc::now()),
        )
        .await
        .context("failed to update document state")?
        .context("missing updated document state")?;
        assert_eq!(deleted_document.document_state, "deleted");
        assert!(deleted_document.deleted_at.is_some());

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn content_repository_keeps_one_logical_document_per_canonical_url_inside_library()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let canonical_url = "https://docs.example.test/reference/accounts".to_string();
        let document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &canonical_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create canonical web document")?;

        let first_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "web_page",
                checksum: "sha256:web-rev-1",
                mime_type: "text/markdown",
                byte_size: 256,
                title: Some("Accounts Reference"),
                language_code: Some("en"),
                source_uri: Some(&canonical_url),
                storage_key: Some("web/accounts-rev-1"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create first canonical web revision")?;
        let second_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 2,
                parent_revision_id: Some(first_revision.id),
                content_source_kind: "web_page",
                checksum: "sha256:web-rev-2",
                mime_type: "text/markdown",
                byte_size: 384,
                title: Some("Accounts Reference"),
                language_code: Some("en"),
                source_uri: Some(&canonical_url),
                storage_key: Some("web/accounts-rev-2"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create second canonical web revision")?;

        let fetched = content_repository::get_document_by_external_key(
            &pool,
            fixture.library_id,
            &canonical_url,
        )
        .await
        .context("failed to fetch document by canonical url")?
        .context("missing canonical web document")?;
        assert_eq!(fetched.id, document.id);

        let revisions = content_repository::list_revisions_by_document(&pool, document.id)
            .await
            .context("failed to list revisions for canonical web document")?;
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].id, second_revision.id);
        assert_eq!(revisions[1].id, first_revision.id);

        let duplicate_error = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &canonical_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .expect_err("same canonical url must stay one logical document per library");
        assert!(
            duplicate_error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
            "expected unique violation, got {duplicate_error:?}"
        );

        let secondary_library_id = sqlx::query_scalar::<_, Uuid>(
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
        .bind(fixture.workspace_id)
        .bind(format!("content-library-secondary-{}", Uuid::now_v7().simple()))
        .bind("Content Repository Secondary Library")
        .bind("secondary canonical web document scope")
        .bind(fixture.principal_id)
        .fetch_one(&pool)
        .await
        .context("failed to create secondary library")?;

        let secondary_document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: secondary_library_id,
                external_key: &canonical_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create canonical web document in secondary library")?;
        assert_ne!(secondary_document.id, document.id);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn content_repository_tracks_mutation_idempotency_and_items() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &format!("mutation-doc-{}", Uuid::now_v7()),
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create document for mutation flow")?;
        let base_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: "sha256:mutation-base",
                mime_type: "text/plain",
                byte_size: 100,
                title: Some("Base"),
                language_code: None,
                source_uri: None,
                storage_key: Some("storage/mutation-base"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create base revision")?;
        let result_revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 2,
                parent_revision_id: Some(base_revision.id),
                content_source_kind: "replace",
                checksum: "sha256:mutation-result",
                mime_type: "text/plain",
                byte_size: 130,
                title: Some("Result"),
                language_code: None,
                source_uri: None,
                storage_key: Some("storage/mutation-result"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create result revision")?;

        let mutation = content_repository::create_mutation(
            &pool,
            &NewContentMutation {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                operation_kind: "replace",
                requested_by_principal_id: Some(fixture.principal_id),
                request_surface: "mcp",
                idempotency_key: Some("mutation-idempotency-key"),
                source_identity: Some("sha256:mutation-result"),
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .context("failed to create content mutation")?;
        let mutation_by_id = content_repository::get_mutation_by_id(&pool, mutation.id)
            .await
            .context("failed to get mutation by id")?
            .context("missing mutation by id")?;
        let mutation_by_key = content_repository::find_mutation_by_idempotency(
            &pool,
            fixture.principal_id,
            "mcp",
            "mutation-idempotency-key",
        )
        .await
        .context("failed to find mutation by idempotency")?
        .context("missing mutation by idempotency")?;
        let listed_mutations =
            content_repository::list_mutations_by_library(&pool, fixture.library_id)
                .await
                .context("failed to list mutations by library")?;
        assert_eq!(mutation_by_id.id, mutation.id);
        assert_eq!(mutation_by_key.id, mutation.id);
        assert!(listed_mutations.iter().any(|row| row.id == mutation.id));

        let item = content_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: Some(document.id),
                base_revision_id: Some(base_revision.id),
                result_revision_id: None,
                item_state: "pending",
                message: Some("queued for apply"),
            },
        )
        .await
        .context("failed to create mutation item")?;
        let updated_item = content_repository::update_mutation_item(
            &pool,
            item.id,
            Some(document.id),
            Some(base_revision.id),
            Some(result_revision.id),
            "applied",
            Some("applied cleanly"),
        )
        .await
        .context("failed to update mutation item")?
        .context("missing updated mutation item")?;
        let item_by_id = content_repository::get_mutation_item_by_id(&pool, item.id)
            .await
            .context("failed to get mutation item by id")?
            .context("missing mutation item by id")?;
        let listed_items = content_repository::list_mutation_items(&pool, mutation.id)
            .await
            .context("failed to list mutation items")?;
        assert_eq!(updated_item.item_state, "applied");
        assert_eq!(item_by_id.result_revision_id, Some(result_revision.id));
        assert_eq!(listed_items.len(), 1);
        assert_eq!(listed_items[0].id, item.id);

        let applied_mutation = content_repository::update_mutation_status(
            &pool,
            mutation.id,
            "applied",
            Some(Utc::now()),
            None,
            None,
        )
        .await
        .context("failed to update mutation status")?
        .context("missing applied mutation")?;
        assert_eq!(applied_mutation.mutation_state, "applied");
        assert!(applied_mutation.completed_at.is_some());

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
