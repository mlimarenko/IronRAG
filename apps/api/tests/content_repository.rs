use anyhow::Context;
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        catalog_repository, content_repository,
        content_repository::{
            NewContentChunk, NewContentDocument, NewContentDocumentHead, NewContentMutation,
            NewContentMutationItem, NewContentRevision,
        },
        iam_repository, ingest_repository,
        ingest_repository::{NewIngestAttempt, NewIngestJob},
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
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
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
                document_hint: None,
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
                document_hint: None,
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
                occurred_at: None,
                occurred_until: None,
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
                occurred_at: None,
                occurred_until: None,
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

        let source_truth_before_delete =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
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
        let source_truth_after_delete =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(
            source_truth_after_delete > source_truth_before_delete,
            "a direct readable-document state change must invalidate answer cache identities",
        );

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn readable_fingerprint_fails_closed_when_revision_or_chunk_projection_diverges()
-> anyhow::Result<()> {
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
                external_key: "projection-parity-document",
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;
        let revision = content_repository::create_revision(
            &pool,
            &NewContentRevision {
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: "sha256:projection-parity-revision",
                mime_type: "text/plain",
                byte_size: 31,
                title: Some("Projection parity"),
                language_code: Some("en"),
                source_uri: Some("file:///projection-parity.txt"),
                document_hint: Some("Neutral projection hint"),
                storage_key: Some("projection/parity/source"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await?;
        let normalized_text = "A durable projection remains coherent.";
        let chunk_checksum = hex::encode(Sha256::digest(normalized_text.as_bytes()));
        let chunk = content_repository::create_chunk(
            &pool,
            &NewContentChunk {
                revision_id: revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: i32::try_from(normalized_text.len())?,
                token_count: Some(5),
                normalized_text,
                text_checksum: &chunk_checksum,
                occurred_at: None,
                occurred_until: None,
            },
        )
        .await?;

        sqlx::query(
            "insert into knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role,
                created_at, updated_at, deleted_at
             ) values (
                $1, $2, $3, $4, $5, $6, 'active', $7, $7, 1, null, 'primary',
                now(), now(), null
             )",
        )
        .bind(document.id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(&document.external_key)
        .bind("projection-parity.txt")
        .bind(&revision.title)
        .bind(revision.id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint,
                mime_type, checksum, title, byte_size, normalized_text, text_checksum,
                image_checksum, text_state, vector_state, graph_state, text_readable_at,
                vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at
             ) values (
                $1, $2, $3, $4, 1, 'ready', 'upload', $5, $6, $7, $8, $9, $10, $11,
                $12, $13, null, 'ready', 'ready', 'ready', now(), now(), now(), null, now()
             )",
        )
        .bind(revision.id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(document.id)
        .bind(&revision.storage_key)
        .bind(&revision.source_uri)
        .bind(&revision.document_hint)
        .bind(&revision.mime_type)
        .bind(&revision.checksum)
        .bind(&revision.title)
        .bind(revision.byte_size)
        .bind(normalized_text)
        .bind(&chunk_checksum)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into knowledge_chunk (
                chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index,
                chunk_kind, content_text, normalized_text, span_start, span_end, token_count,
                support_block_ids, section_path, heading_trail, literal_digest, chunk_state,
                text_generation, vector_generation, quality_score, window_text, raptor_level,
                occurred_at, occurred_until
             ) values (
                $1, $2, $3, $4, $5, 0, 'paragraph', $6, $6, 0, $7, 5,
                '{}', '{}', '{}', $8, 'ready', 1, 1, 1.0, null, null, null, null
             )",
        )
        .bind(chunk.id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(document.id)
        .bind(revision.id)
        .bind(normalized_text)
        .bind(i32::try_from(normalized_text.len())?)
        .bind(format!("sha256:{chunk_checksum}"))
        .execute(&pool)
        .await?;
        content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;

        let baseline =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(baseline.projection_is_current);

        // The durable generation is the invalidation contract. Without a
        // generation transition, a process hit must not repeat the O(chunks)
        // parity scan on every query.
        sqlx::query(
            "update knowledge_revision set title = 'Diverged title' where revision_id = $1",
        )
        .bind(revision.id)
        .execute(&pool)
        .await?;
        let generation_hit =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert_eq!(generation_hit, baseline);

        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let revision_divergence =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(!revision_divergence.projection_is_current);

        sqlx::query("update knowledge_revision set title = $2 where revision_id = $1")
            .bind(revision.id)
            .bind(&revision.title)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(
            content_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?
            .projection_is_current
        );

        sqlx::query(
            "update knowledge_chunk
             set normalized_text = 'Projection text diverged.'
             where chunk_id = $1",
        )
        .bind(chunk.id)
        .execute(&pool)
        .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let chunk_divergence =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(!chunk_divergence.projection_is_current);

        sqlx::query("update knowledge_chunk set normalized_text = $2 where chunk_id = $1")
            .bind(chunk.id)
            .bind(normalized_text)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let converged =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(converged.projection_is_current);

        let displaced_chunk_id = Uuid::now_v7();
        sqlx::query("update knowledge_chunk set chunk_id = $2 where chunk_id = $1")
            .bind(chunk.id)
            .bind(displaced_chunk_id)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(
            !content_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?
            .projection_is_current,
            "canonical and projected chunk identities must match exactly",
        );
        sqlx::query("update knowledge_chunk set chunk_id = $2 where chunk_id = $1")
            .bind(displaced_chunk_id)
            .bind(chunk.id)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;

        sqlx::query("update content_chunk set text_checksum = 'invalid' where id = $1")
            .bind(chunk.id)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(
            !content_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?
            .projection_is_current,
            "canonical chunk checksums must authenticate projected normalized text",
        );
        sqlx::query("update content_chunk set text_checksum = $2 where id = $1")
            .bind(chunk.id)
            .bind(&chunk_checksum)
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let checksum_repaired =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(checksum_repaired.projection_is_current);

        sqlx::query("update knowledge_chunk set content_text = $2 where chunk_id = $1")
            .bind(chunk.id)
            .bind("Answer-visible formatting changed.")
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let answer_visible_projection_change =
            content_repository::get_library_readable_content_fingerprint(&pool, fixture.library_id)
                .await?;
        assert!(answer_visible_projection_change.projection_is_current);
        assert_ne!(
            answer_visible_projection_change.value, checksum_repaired.value,
            "projection-only answer fields must participate in the cache identity",
        );

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_head_rejects_foreign_parentage_and_only_bumps_for_readable_transitions()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let foreign_fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: "head-parentage-target",
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;
        let sibling = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: "head-parentage-sibling",
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;
        let foreign_document = content_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: foreign_fixture.workspace_id,
                library_id: foreign_fixture.library_id,
                external_key: "head-parentage-foreign",
                document_state: "active",
                created_by_principal_id: Some(foreign_fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;

        let revision_for =
            |document_id, workspace_id, library_id, checksum: &'static str| NewContentRevision {
                document_id,
                workspace_id,
                library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum,
                mime_type: "text/plain",
                byte_size: 1,
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            };
        let readable_revision = content_repository::create_revision(
            &pool,
            &revision_for(
                document.id,
                fixture.workspace_id,
                fixture.library_id,
                "sha256:head-target",
            ),
        )
        .await?;
        let sibling_revision = content_repository::create_revision(
            &pool,
            &revision_for(
                sibling.id,
                fixture.workspace_id,
                fixture.library_id,
                "sha256:head-sibling",
            ),
        )
        .await?;
        let foreign_revision = content_repository::create_revision(
            &pool,
            &revision_for(
                foreign_document.id,
                foreign_fixture.workspace_id,
                foreign_fixture.library_id,
                "sha256:head-foreign",
            ),
        )
        .await?;

        let initial_generation =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: None,
                readable_revision_id: None,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            initial_generation,
            "creating an empty shell head is not answer-visible",
        );

        content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(readable_revision.id),
                readable_revision_id: None,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            initial_generation,
            "an unreadable active work-in-progress revision is not answer-visible",
        );

        content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(readable_revision.id),
                readable_revision_id: Some(readable_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;
        let readable_generation =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(readable_generation > initial_generation);

        let mutation = content_repository::create_mutation(
            &pool,
            &NewContentMutation {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                operation_kind: "replace",
                requested_by_principal_id: Some(fixture.principal_id),
                request_surface: "rest",
                idempotency_key: None,
                source_identity: None,
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await?;
        let primary_mutation_item = content_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: Some(document.id),
                base_revision_id: Some(readable_revision.id),
                result_revision_id: Some(readable_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        let sibling_mutation_item = content_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: Some(sibling.id),
                base_revision_id: Some(sibling_revision.id),
                result_revision_id: Some(sibling_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        let job = ingest_repository::create_ingest_job(
            &pool,
            &NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: Some(mutation.id),
                mutation_item_id: Some(primary_mutation_item.id),
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document.id),
                knowledge_revision_id: Some(readable_revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 100,
                dedupe_key: None,
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await?;
        let attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "running".to_string(),
                current_stage: Some("finalizing".to_string()),
                started_at: None,
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        content_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(readable_revision.id),
                readable_revision_id: Some(readable_revision.id),
                latest_mutation_id: Some(mutation.id),
                latest_successful_attempt_id: Some(attempt.id),
            },
        )
        .await?;
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            readable_generation,
            "operational head ids and no-op pointers must not churn answer generations",
        );

        let aggregate_sibling_job = ingest_repository::create_ingest_job(
            &pool,
            &NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: Some(mutation.id),
                mutation_item_id: Some(sibling_mutation_item.id),
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(sibling.id),
                knowledge_revision_id: Some(sibling_revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 100,
                dedupe_key: None,
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await?;
        let aggregate_sibling_attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &NewIngestAttempt {
                job_id: aggregate_sibling_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "running".to_string(),
                current_stage: Some("finalizing".to_string()),
                started_at: None,
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        assert!(
            content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(readable_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: Some(aggregate_sibling_attempt.id),
                },
            )
            .await
            .is_err(),
            "an aggregate mutation must not make another document's attempt transferable",
        );

        for invalid_revision in [sibling_revision.id, foreign_revision.id] {
            let invalid = content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(invalid_revision),
                    readable_revision_id: Some(invalid_revision),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: Some(attempt.id),
                },
            )
            .await;
            assert!(invalid.is_err(), "foreign revision parentage must be rejected");
        }

        let sibling_mutation = content_repository::create_mutation(
            &pool,
            &NewContentMutation {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                operation_kind: "replace",
                requested_by_principal_id: Some(fixture.principal_id),
                request_surface: "rest",
                idempotency_key: None,
                source_identity: None,
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await?;
        let standalone_sibling_item = content_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: sibling_mutation.id,
                document_id: Some(sibling.id),
                base_revision_id: Some(sibling_revision.id),
                result_revision_id: Some(sibling_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        assert!(
            content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(readable_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(sibling_mutation.id),
                    latest_successful_attempt_id: Some(attempt.id),
                },
            )
            .await
            .is_err(),
            "a same-library mutation anchored to another document must be rejected",
        );

        let foreign_mutation = content_repository::create_mutation(
            &pool,
            &NewContentMutation {
                workspace_id: foreign_fixture.workspace_id,
                library_id: foreign_fixture.library_id,
                operation_kind: "replace",
                requested_by_principal_id: Some(foreign_fixture.principal_id),
                request_surface: "rest",
                idempotency_key: None,
                source_identity: None,
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await?;
        let foreign_mutation_item = content_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: foreign_mutation.id,
                document_id: Some(foreign_document.id),
                base_revision_id: Some(foreign_revision.id),
                result_revision_id: Some(foreign_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        assert!(
            content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(readable_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(foreign_mutation.id),
                    latest_successful_attempt_id: Some(attempt.id),
                },
            )
            .await
            .is_err(),
            "a cross-tenant mutation must be rejected",
        );

        let sibling_job = ingest_repository::create_ingest_job(
            &pool,
            &NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: Some(sibling_mutation.id),
                mutation_item_id: Some(standalone_sibling_item.id),
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(sibling.id),
                knowledge_revision_id: Some(sibling_revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 100,
                dedupe_key: None,
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await?;
        let sibling_attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &NewIngestAttempt {
                job_id: sibling_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "running".to_string(),
                current_stage: Some("finalizing".to_string()),
                started_at: None,
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        let foreign_job = ingest_repository::create_ingest_job(
            &pool,
            &NewIngestJob {
                workspace_id: foreign_fixture.workspace_id,
                library_id: foreign_fixture.library_id,
                mutation_id: Some(foreign_mutation.id),
                mutation_item_id: Some(foreign_mutation_item.id),
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(foreign_document.id),
                knowledge_revision_id: Some(foreign_revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 100,
                dedupe_key: None,
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await?;
        let foreign_attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &NewIngestAttempt {
                job_id: foreign_job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: None,
                knowledge_generation_id: None,
                attempt_state: "running".to_string(),
                current_stage: Some("finalizing".to_string()),
                started_at: None,
                heartbeat_at: None,
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 90,
                retryable: false,
            },
        )
        .await?;
        assert!(
            content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(readable_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: Some(sibling_attempt.id),
                },
            )
            .await
            .is_err(),
            "an attempt owned by another document must be rejected",
        );
        assert!(
            content_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: Some(readable_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: Some(foreign_attempt.id),
                },
            )
            .await
            .is_err(),
            "a cross-tenant attempt must be rejected",
        );

        let stable_head = content_repository::get_document_head(&pool, document.id)
            .await?
            .context("target head missing")?;
        assert_eq!(stable_head.readable_revision_id, Some(readable_revision.id));
        assert_eq!(stable_head.latest_mutation_id, Some(mutation.id));
        assert_eq!(stable_head.latest_successful_attempt_id, Some(attempt.id));
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            readable_generation,
        );

        Ok(())
    }
    .await;

    foreign_fixture.cleanup(&pool).await?;
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
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
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
                document_hint: None,
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
                document_hint: None,
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
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
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
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
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
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
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
                document_hint: None,
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
                document_hint: None,
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
