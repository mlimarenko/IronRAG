use crate::temp_postgres_database;

use anyhow::Context;
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        catalog_repository,
        content_chunk_repository::{self, NewContentChunk},
        content_document_head_repository::{self, NewContentDocumentHead},
        content_document_list_repository,
        content_document_repository::{self, NewContentDocument},
        content_fingerprint_repository,
        content_mutation_repository::{self, NewContentMutation, NewContentMutationItem},
        content_revision_repository::{self, NewContentRevision},
        iam_repository, ingest_repository,
        ingest_repository::{NewIngestAttempt, NewIngestJob},
    },
};

const TEST_INGEST_PRIORITY: i32 = 100;

pub(crate) fn synthetic_import_admission(
    replacement: ironrag_contracts::documents::ReplacementIdentity,
) -> anyhow::Result<ironrag_contracts::documents::RevisionAdmissionIdentity> {
    use ironrag_contracts::documents::{ImportAdmissionProvenance, RevisionAdmissionIdentity};
    // Synthetic integration-test evidence only, not a production recovery certificate.
    Ok(RevisionAdmissionIdentity::CertifiedImport {
        replacement,
        provenance: ImportAdmissionProvenance {
            recovery_snapshot_id: "1".repeat(64),
            identity_certificate_sha256: "2".repeat(64),
            source_baseline_sha384: "3".repeat(96),
            admitted_by: "synthetic-integration-test-fixture".to_owned(),
        },
    }
    .normalize()?)
}

pub(crate) fn unbound_mutation_admission(
    replacement: ironrag_contracts::documents::ReplacementIdentity,
) -> anyhow::Result<ironrag_contracts::documents::RevisionAdmissionIdentity> {
    Ok(ironrag_contracts::documents::RevisionAdmissionIdentity::Mutation {
        source_identity: None,
        materialization_token: None,
        visibility_intent_id: None,
        replacement,
    }
    .normalize()?)
}

pub(crate) async fn create_imported_fixture_revision(
    postgres: &PgPool,
    revision: &NewContentRevision<'_>,
    file_name: &str,
) -> anyhow::Result<content_revision_repository::ContentRevisionRow> {
    let admission =
        synthetic_import_admission(ironrag_contracts::documents::ReplacementIdentity {
            checksum: revision.checksum.to_owned(),
            byte_size: revision.byte_size,
            mime_type: revision.mime_type.to_owned(),
            file_name: file_name.to_owned(),
            title: revision.title.map(str::to_owned),
            language_code: revision.language_code.map(str::to_owned),
            source_uri: revision.source_uri.map(str::to_owned),
            document_hint: revision.document_hint.map(str::to_owned),
        })?;
    Ok(content_revision_repository::create_revision(
        postgres,
        &NewContentRevision { admission_identity: Some(&admission), ..revision.clone() },
    )
    .await?)
}

pub(crate) async fn imported_knowledge_revision_admission(
    postgres: &PgPool,
    revision_id: Uuid,
    file_name: &str,
) -> anyhow::Result<serde_json::Value> {
    let (checksum, byte_size, mime_type, title, source_uri, document_hint):
        (String, i64, String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "select checksum, byte_size, mime_type, title, source_uri, document_hint from knowledge_revision where revision_id=$1",
        ).bind(revision_id).fetch_one(postgres).await?;
    let admission =
        synthetic_import_admission(ironrag_contracts::documents::ReplacementIdentity {
            checksum,
            byte_size,
            mime_type,
            file_name: file_name.to_owned(),
            title,
            language_code: None,
            source_uri,
            document_hint,
        })?;
    Ok(serde_json::to_value(admission)?)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_revision_counter_survives_pruning_and_exhaustion() -> anyhow::Result<()>
{
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let document = content_document_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: "retained-counter",
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;
        let revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 20,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: "sha256:counter-fixture",
                mime_type: "text/plain",
                byte_size: 0,
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await?;
        sqlx::query("delete from content_revision where id = $1")
            .bind(revision.id)
            .execute(&pool)
            .await?;
        let mut transaction = pool.begin().await?;
        catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *transaction,
            fixture.workspace_id,
            fixture.library_id,
        )
        .await?;
        content_document_repository::lock_document_by_id_with_executor(
            &mut *transaction,
            document.id,
        )
        .await?;
        let next = content_revision_repository::allocate_revision_number_with_executor(
            &mut *transaction,
            document.id,
        )
        .await?;
        assert_eq!(next, 21, "pruning cannot reuse or recapture a revision floor");
        transaction.rollback().await?;
        let retained: i32 =
            sqlx::query_scalar("select revision_sequence from content_document where id = $1")
                .bind(document.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(retained, 20, "failed admission rolls back its counter allocation");
        sqlx::query("update content_document set revision_sequence = 2147483647 where id = $1")
            .bind(document.id)
            .execute(&pool)
            .await?;
        let failure =
            content_revision_repository::allocate_revision_number_with_executor(&pool, document.id)
                .await
                .expect_err("integer exhaustion must reject allocation");
        assert!(matches!(failure, sqlx::Error::Protocol(_)));
        let exhausted: i32 =
            sqlx::query_scalar("select revision_sequence from content_document where id = $1")
                .bind(document.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(exhausted, i32::MAX);
        Ok(())
    }
    .await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

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
        .bind("authoritative content repository tests")
        .bind(principal.id)
        .fetch_one(pool)
        .await
        .context("failed to insert content repository library")?;

        Ok(Self { principal_id: principal.id, workspace_id, library_id })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        // `content_mutation_item.document_id` is ON DELETE RESTRICT, so a bare
        // `delete from catalog_workspace` fails as soon as a test has admitted
        // a mutation. The repository owns that ordering; duplicating it here
        // is what let this fixture drift out of sync with the schema.
        catalog_repository::delete_workspace(pool, self.workspace_id)
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

async fn connect_postgres(
    settings: &Settings,
) -> anyhow::Result<(PgPool, temp_postgres_database::TempPostgresDatabase)> {
    let temp_database = temp_postgres_database::TempPostgresDatabase::create(
        &settings.database_url,
        "content_repository",
    )
    .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect content repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for content repository test")?;
    Ok((pool, temp_database))
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn content_repository_persists_logical_document_revision_head_and_chunks()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let external_key = format!("doc-{}", Uuid::now_v7());
        let document = content_document_repository::create_document(
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

        let by_id = content_document_repository::get_document_by_id(&pool, document.id)
            .await
            .context("failed to load document by id")?
            .context("missing document by id")?;
        let listed =
            content_document_repository::list_documents_by_library(&pool, fixture.library_id)
                .await
                .context("failed to list documents by library")?;
        assert_eq!(by_id.id, document.id);
        assert!(listed.iter().any(|row| row.id == document.id));

        let first_revision = create_imported_fixture_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "1".repeat(64)),
                mime_type: "text/plain",
                byte_size: 128,
                title: Some("Revision One"),
                language_code: Some("en"),
                source_uri: Some("file:///doc-1.txt"),
                document_hint: None,
                storage_key: Some("storage/doc-1"),
                created_by_principal_id: Some(fixture.principal_id),
            },
            "repository-source.txt",
        )
        .await
        .context("failed to create first revision")?;
        let second_revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
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
            content_revision_repository::get_latest_revision_for_document(&pool, document.id)
                .await
                .context("failed to load latest revision")?
                .context("missing latest revision")?;
        let revisions = content_revision_repository::list_revisions_by_document(&pool, document.id)
            .await
            .context("failed to list revisions by document")?;
        assert_eq!(latest_revision.id, second_revision.id);
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].revision_number, 2);
        assert_eq!(revisions[1].revision_number, 1);

        let initial_head = content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
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

        let updated_head = content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
                document_id: document.id,
                active_revision_id: Some(second_revision.id),
                readable_revision_id: Some(first_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to update document head")?;
        let loaded_head = content_document_head_repository::get_document_head(&pool, document.id)
            .await
            .context("failed to load document head")?
            .context("missing document head")?;
        assert_eq!(updated_head.active_revision_id, Some(second_revision.id));
        assert_eq!(loaded_head.readable_revision_id, Some(first_revision.id));

        let first_chunk = content_chunk_repository::create_chunk(
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
        let second_chunk = content_chunk_repository::create_chunk(
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

        let chunk_by_id = content_chunk_repository::get_chunk_by_id(&pool, first_chunk.id)
            .await
            .context("failed to get chunk by id")?
            .context("missing content chunk")?;
        let listed_chunks =
            content_chunk_repository::list_chunks_by_revision(&pool, second_revision.id)
                .await
                .context("failed to list chunks by revision")?;
        assert_eq!(chunk_by_id.chunk_index, 0);
        assert_eq!(listed_chunks.len(), 2);
        assert_eq!(listed_chunks[0].id, first_chunk.id);
        assert_eq!(listed_chunks[1].id, second_chunk.id);

        let deleted =
            content_chunk_repository::delete_chunks_by_revision(&pool, second_revision.id)
                .await
                .context("failed to delete chunks by revision")?;
        let remaining_chunks =
            content_chunk_repository::list_chunks_by_revision(&pool, second_revision.id)
                .await
                .context("failed to re-list chunks after delete")?;
        assert_eq!(deleted, 2);
        assert!(remaining_chunks.is_empty());

        let source_truth_before_delete =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let deleted_document = content_document_repository::update_document_state(
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn readable_identity_uses_generation_fence_and_detects_metadata_drift() -> anyhow::Result<()>
{
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let empty_identity =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(!empty_identity.has_readable_content);

        let document = content_document_repository::create_document(
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
        let revision = create_imported_fixture_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "4".repeat(64)),
                mime_type: "text/plain",
                byte_size: 31,
                title: Some("Projection parity"),
                language_code: Some("en"),
                source_uri: Some("file:///projection-parity.txt"),
                document_hint: Some("Neutral projection hint"),
                storage_key: Some("projection/parity/source"),
                created_by_principal_id: Some(fixture.principal_id),
            },
            "repository-source.txt",
        )
        .await?;
        let normalized_text = "A durable projection remains coherent.";
        let chunk_checksum = hex::encode(Sha256::digest(normalized_text.as_bytes()));
        let chunk = content_chunk_repository::create_chunk(
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
                $12, $13, null, 'text_readable', 'ready', 'ready', now(), now(), now(), null, now()
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
                text_generation, vector_generation, quality_score, window_text,
                occurred_at, occurred_until
             ) values (
                $1, $2, $3, $4, $5, 0, 'paragraph', $6, $6, 0, $7, 5,
                '{}', '{}', '{}', $8, 'ready', 1, 1, 1.0, null, null, null
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
        sqlx::query(
            "insert into knowledge_structured_revision (
                revision_id, workspace_id, library_id, document_id,
                preparation_state, normalization_profile, source_format, language_code,
                block_count, chunk_count, typed_fact_count, outline_json,
                prepared_at, updated_at
             ) values (
                $1, $2, $3, $4, 'prepared', 'fixture-v1', 'text', null,
                0, 1, 0, '[]'::jsonb, now(), now()
             )",
        )
        .bind(revision.id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(document.id)
        .execute(&pool)
        .await?;
        content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;

        let baseline = content_fingerprint_repository::get_library_readable_content_fingerprint(
            &pool,
            fixture.library_id,
        )
        .await?;
        assert!(!baseline.is_catching_up);
        assert!(baseline.has_readable_content);
        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'preparing'
             where revision_id = $1",
        )
        .bind(revision.id)
        .execute(&pool)
        .await?;
        let preparing_identity =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(
            !preparing_identity.has_readable_content,
            "a readable pointer must not expose a partially replaced structured artifact",
        );
        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'prepared'
             where revision_id = $1",
        )
        .bind(revision.id)
        .execute(&pool)
        .await?;

        let active_job = ingest_repository::create_ingest_job(
            &pool,
            &NewIngestJob {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document.id),
                knowledge_revision_id: Some(revision.id),
                job_kind: "graph_refresh".to_string(),
                queue_state: "queued".to_string(),
                priority: TEST_INGEST_PRIORITY,
                dedupe_key: None,
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await?;
        let active_ingest_identity =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(active_ingest_identity.is_catching_up);
        assert!(active_ingest_identity.has_readable_content);

        sqlx::query(
            "update ingest_job
             set queue_state = 'completed', completed_at = now()
             where id = $1",
        )
        .bind(active_job.id)
        .execute(&pool)
        .await?;
        let completed_ingest_identity =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(!completed_ingest_identity.is_catching_up);

        let repeated_completed_identity =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert_eq!(repeated_completed_identity, completed_ingest_identity);

        sqlx::query(
            "update knowledge_revision set title = 'Diverged title' where revision_id = $1",
        )
        .bind(revision.id)
        .execute(&pool)
        .await?;
        let revision_divergence =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(revision_divergence.is_catching_up);
        assert_eq!(revision_divergence.value, completed_ingest_identity.value);

        sqlx::query("update knowledge_revision set title = $2 where revision_id = $1")
            .bind(revision.id)
            .bind(&revision.title)
            .execute(&pool)
            .await?;
        assert!(
            !content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?
            .is_catching_up
        );

        let repaired = content_fingerprint_repository::get_library_readable_content_fingerprint(
            &pool,
            fixture.library_id,
        )
        .await?;

        sqlx::query("update knowledge_chunk set content_text = $2 where chunk_id = $1")
            .bind(chunk.id)
            .bind("Answer-visible formatting changed.")
            .execute(&pool)
            .await?;
        catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id).await?;
        let answer_visible_projection_change =
            content_fingerprint_repository::get_library_readable_content_fingerprint(
                &pool,
                fixture.library_id,
            )
            .await?;
        assert!(!answer_visible_projection_change.is_catching_up);
        assert_ne!(
            answer_visible_projection_change.value, repaired.value,
            "answer-visible writes must advance the cache generation",
        );

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
async fn document_head_rejects_foreign_parentage_and_only_bumps_for_readable_transitions()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let foreign_fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let document = content_document_repository::create_document(
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
        let sibling = content_document_repository::create_document(
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
        let foreign_document = content_document_repository::create_document(
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
                admission_identity: None,
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
        let readable_revision = create_imported_fixture_revision(
            &pool,
            &revision_for(
                document.id,
                fixture.workspace_id,
                fixture.library_id,
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            "repository-source.txt",
        )
        .await?;
        let sibling_revision = content_revision_repository::create_revision(
            &pool,
            &revision_for(
                sibling.id,
                fixture.workspace_id,
                fixture.library_id,
                "sha256:head-sibling",
            ),
        )
        .await?;
        let foreign_revision = content_revision_repository::create_revision(
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
        content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
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

        content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
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

        content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
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

        let request_identity = replacement_mutation_identity(
            &readable_revision,
            Some(readable_revision.id),
            None,
            "repository-source.txt",
        );
        let mutation = content_mutation_repository::create_mutation(
            &pool,
            &NewContentMutation {
                request_identity: &request_identity,
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
        let primary_mutation_item = content_mutation_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: document.id,
                base_revision_id: Some(readable_revision.id),
                result_revision_id: Some(readable_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        let sibling_mutation_item = content_mutation_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: sibling.id,
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
        let attempt =
            ingest_repository::create_ingest_attempt(&pool, &running_finalization_attempt(job.id))
                .await?;
        content_document_head_repository::upsert_document_head(
            &pool,
            &repository_readable_head(
                &pool,
                document.id,
                readable_revision.id,
                mutation.id,
                attempt.id,
            )
            .await?,
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
            &running_finalization_attempt(aggregate_sibling_job.id),
        )
        .await?;
        assert!(
            content_document_head_repository::upsert_document_head(
                &pool,
                &repository_readable_head(
                    &pool,
                    document.id,
                    readable_revision.id,
                    mutation.id,
                    aggregate_sibling_attempt.id
                )
                .await?,
            )
            .await
            .is_err(),
            "an aggregate mutation must not make another document's attempt transferable",
        );

        for invalid_revision in [sibling_revision.id, foreign_revision.id] {
            let invalid = content_document_head_repository::upsert_document_head(
                &pool,
                &NewContentDocumentHead {
                    publishing_mutation_item_id: None,
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

        let sibling_mutation = content_mutation_repository::create_mutation(
            &pool,
            &NewContentMutation {
                request_identity: &replacement_mutation_identity(
                    &sibling_revision,
                    Some(sibling_revision.id),
                    None,
                    "repository-source.txt",
                ),
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
        let standalone_sibling_item = content_mutation_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: sibling_mutation.id,
                document_id: sibling.id,
                base_revision_id: Some(sibling_revision.id),
                result_revision_id: Some(sibling_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        assert!(
            content_document_head_repository::upsert_document_head(
                &pool,
                &repository_readable_head(
                    &pool,
                    document.id,
                    readable_revision.id,
                    sibling_mutation.id,
                    attempt.id
                )
                .await?,
            )
            .await
            .is_err(),
            "a same-library mutation anchored to another document must be rejected",
        );

        let foreign_mutation = content_mutation_repository::create_mutation(
            &pool,
            &NewContentMutation {
                request_identity: &replacement_mutation_identity(
                    &foreign_revision,
                    Some(foreign_revision.id),
                    None,
                    "repository-source.txt",
                ),
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
        let foreign_mutation_item = content_mutation_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: foreign_mutation.id,
                document_id: foreign_document.id,
                base_revision_id: Some(foreign_revision.id),
                result_revision_id: Some(foreign_revision.id),
                item_state: "pending",
                message: None,
            },
        )
        .await?;
        assert!(
            content_document_head_repository::upsert_document_head(
                &pool,
                &repository_readable_head(
                    &pool,
                    document.id,
                    readable_revision.id,
                    foreign_mutation.id,
                    attempt.id
                )
                .await?,
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
            &running_finalization_attempt(sibling_job.id),
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
            &running_finalization_attempt(foreign_job.id),
        )
        .await?;
        assert!(
            content_document_head_repository::upsert_document_head(
                &pool,
                &repository_readable_head(
                    &pool,
                    document.id,
                    readable_revision.id,
                    mutation.id,
                    sibling_attempt.id
                )
                .await?,
            )
            .await
            .is_err(),
            "an attempt owned by another document must be rejected",
        );
        assert!(
            content_document_head_repository::upsert_document_head(
                &pool,
                &repository_readable_head(
                    &pool,
                    document.id,
                    readable_revision.id,
                    mutation.id,
                    foreign_attempt.id
                )
                .await?,
            )
            .await
            .is_err(),
            "a cross-tenant attempt must be rejected",
        );

        let stable_head = content_document_head_repository::get_document_head(&pool, document.id)
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn content_repository_keeps_one_logical_document_per_authoritative_url_inside_library()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let authoritative_url = "https://docs.example.test/reference/accounts".to_string();
        let document = content_document_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &authoritative_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .context("failed to create authoritative web document")?;

        let first_revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
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
                source_uri: Some(&authoritative_url),
                document_hint: None,
                storage_key: Some("web/accounts-rev-1"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create first authoritative web revision")?;
        let second_revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
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
                source_uri: Some(&authoritative_url),
                document_hint: None,
                storage_key: Some("web/accounts-rev-2"),
                created_by_principal_id: Some(fixture.principal_id),
            },
        )
        .await
        .context("failed to create second authoritative web revision")?;

        let fetched = content_document_repository::get_document_by_external_key(
            &pool,
            fixture.library_id,
            &authoritative_url,
        )
        .await
        .context("failed to fetch document by preferred URL")?
        .context("missing authoritative web document")?;
        assert_eq!(fetched.id, document.id);

        let revisions = content_revision_repository::list_revisions_by_document(&pool, document.id)
            .await
            .context("failed to list revisions for authoritative web document")?;
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].id, second_revision.id);
        assert_eq!(revisions[1].id, first_revision.id);

        let duplicate_error = content_document_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: &authoritative_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .expect_err("same preferred URL must stay one logical document per library");
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
        .bind("secondary authoritative web document scope")
        .bind(fixture.principal_id)
        .fetch_one(&pool)
        .await
        .context("failed to create secondary library")?;

        let secondary_document = content_document_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: secondary_library_id,
                external_key: &authoritative_url,
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .context("failed to create authoritative web document in secondary library")?;
        assert_ne!(secondary_document.id, document.id);

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
async fn content_repository_tracks_mutation_idempotency_and_items() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let document = content_document_repository::create_document(
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
        let base_revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
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
        let result_revision = content_revision_repository::create_revision(
            &pool,
            &NewContentRevision {
                admission_identity: None,
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

        let mutation = content_mutation_repository::create_mutation(
            &pool,
            &NewContentMutation {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                operation_kind: "replace",
                requested_by_principal_id: Some(fixture.principal_id),
                request_surface: "mcp",
                idempotency_key: Some("mutation-idempotency-key"),
                request_identity: &replacement_mutation_identity(
                    &result_revision,
                    Some(base_revision.id),
                    Some("sha256:mutation-result"),
                    "mutation-result.txt",
                ),
                source_identity: Some("sha256:mutation-result"),
                mutation_state: "accepted",
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .context("failed to create content mutation")?;
        let mutation_by_id = content_mutation_repository::get_mutation_by_id(&pool, mutation.id)
            .await
            .context("failed to get mutation by id")?
            .context("missing mutation by id")?;
        let bound_key =
            ironrag_backend::infra::repositories::admission_repository::has_bound_content_key(
                &pool,
                fixture.workspace_id,
                Some(fixture.principal_id),
                "mcp",
                Some("mutation-idempotency-key"),
            )
            .await
            .context("failed to inspect the canonical key binding")?;
        let listed_mutations =
            content_mutation_repository::list_mutations_by_library(&pool, fixture.library_id)
                .await
                .context("failed to list mutations by library")?;
        assert_eq!(mutation_by_id.id, mutation.id);
        assert!(bound_key);
        assert!(
            ironrag_backend::infra::repositories::admission_repository::has_bound_content_key(
                &pool,
                Uuid::now_v7(),
                Some(fixture.principal_id),
                "mcp",
                Some("mutation-idempotency-key"),
            )
            .await?,
            "the same actor's immutable key stays reserved across workspace changes, not replay-authorized"
        );
        assert!(
            !ironrag_backend::infra::repositories::admission_repository::has_bound_content_key(
                &pool,
                Uuid::now_v7(),
                Some(Uuid::now_v7()),
                "mcp",
                Some("mutation-idempotency-key"),
            )
            .await?,
            "another actor in another workspace has neither reservation"
        );
        assert!(listed_mutations.iter().any(|row| row.id == mutation.id));

        let item = content_mutation_repository::create_mutation_item(
            &pool,
            &NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: document.id,
                base_revision_id: Some(base_revision.id),
                result_revision_id: None,
                item_state: "pending",
                message: Some("queued for apply"),
            },
        )
        .await
        .context("failed to create mutation item")?;
        let updated_item = content_mutation_repository::update_mutation_item(
            &pool,
            content_mutation_repository::MutationItemUpdate {
                item_id: item.id,
                document_id: Some(document.id),
                base_revision_id: Some(base_revision.id),
                result_revision_id: Some(result_revision.id),
                item_state: "applied",
                message: Some("applied cleanly"),
            },
        )
        .await
        .context("failed to update mutation item")?
        .context("missing updated mutation item")?;
        let item_by_id = content_mutation_repository::get_mutation_item_by_id(&pool, item.id)
            .await
            .context("failed to get mutation item by id")?
            .context("missing mutation item by id")?;
        let listed_items = content_mutation_repository::list_mutation_items(&pool, mutation.id)
            .await
            .context("failed to list mutation items")?;
        assert_eq!(updated_item.item_state, "applied");
        assert_eq!(item_by_id.result_revision_id, Some(result_revision.id));
        assert_eq!(listed_items.len(), 1);
        assert_eq!(listed_items[0].id, item.id);

        let applied_mutation = content_mutation_repository::update_mutation_status(
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_page_searches_external_key_and_display_file_name_with_keyset()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        // External identity is deliberately the reverse of visible filename
        // order. The API's `file_name` sort must use what the user sees, while
        // keeping the external key as an independent identity field.
        let mut document_ids = Vec::new();
        for (key, file_name) in [
            ("aaa-key", "zeta-visible.txt"),
            ("bbb-key", "middle-visible.txt"),
            ("escaped%_external-key", "alpha-visible.txt"),
        ] {
            let document = content_document_repository::create_document(
                &pool,
                &NewContentDocument {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: key,
                    document_state: "active",
                    created_by_principal_id: Some(fixture.principal_id),
                    parent_external_key: None,
                    parent_document_id: None,
                    document_role: "primary",
                },
            )
            .await
            .context("failed to create keyset-fallback document")?;
            let revision = create_imported_fixture_revision(
                &pool,
                &NewContentRevision {
                    admission_identity: None,
                    document_id: document.id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    revision_number: 1,
                    parent_revision_id: None,
                    content_source_kind: "upload",
                    checksum: &format!("sha256:{}", hex::encode(Sha256::digest(key.as_bytes()))),
                    mime_type: "text/plain",
                    byte_size: 128,
                    title: Some(key),
                    language_code: Some("en"),
                    source_uri: None,
                    document_hint: None,
                    storage_key: Some(key),
                    created_by_principal_id: Some(fixture.principal_id),
                },
                file_name,
            )
            .await
            .context("failed to create keyset-fallback revision")?;
            content_document_head_repository::upsert_document_head(
                &pool,
                &content_document_head_repository::NewContentDocumentHead {
                    publishing_mutation_item_id: None,
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to upsert keyset-fallback document head")?;
            sqlx::query(
                "insert into knowledge_document (
                    document_id, workspace_id, library_id, external_key, file_name, title,
                    document_state, active_revision_id, readable_revision_id, latest_revision_no,
                    parent_document_id, document_role, created_at, updated_at, deleted_at
                 ) values (
                    $1, $2, $3, $4, $5, $5, 'active', $6, $6, 1,
                    null, 'primary', now(), now(), null
                 )",
            )
            .bind(document.id)
            .bind(fixture.workspace_id)
            .bind(fixture.library_id)
            .bind(key)
            .bind(file_name)
            .bind(revision.id)
            .execute(&pool)
            .await
            .context("failed to materialize keyset-fallback knowledge projection")?;
            seed_grid_readable_revision(&pool, revision.id).await?;
            document_ids.push(document.id);
        }

        // Page 1: no cursor. Before this fix, every page beyond the first
        // replayed this exact result because non-created-at sorts had no
        // usable keyset predicate.
        let page1 = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs { limit: 1, ..Default::default() },
        )
        .await
        .context("failed to fetch keyset-fallback page 1")?;
        assert_eq!(page1.rows.len(), 1);
        assert_eq!(
            page1.rows[0].id, document_ids[2],
            "page 1 must use alpha-visible.txt, not the smallest external key"
        );
        assert_eq!(page1.rows[0].projection_file_name.as_deref(), Some("alpha-visible.txt"));
        assert_eq!(page1.rows[0].external_key, "escaped%_external-key");
        assert!(page1.has_more);

        let page1_cursor = content_document_list_repository::DocumentListPageCursor {
            created_at: page1.rows[0].created_at,
            document_id: page1.rows[0].id,
            text_value: page1.rows[0].projection_file_name.as_ref().map(|name| name.to_lowercase()),
            number_value: None,
            finished_value: None,
            value_is_null: false,
        };
        // Page 2 must use the last row's complete sort tuple and return the
        // next document rather than replaying page 1.
        let page2 = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs { cursor: Some(&page1_cursor), limit: 1, ..Default::default() },
        )
        .await
        .context("failed to fetch keyset-fallback page 2")?;
        assert_eq!(page2.rows.len(), 1);
        assert_eq!(
            page2.rows[0].id, document_ids[1],
            "page 2 must be middle-visible.txt, not repeat page 1"
        );
        assert_ne!(page2.rows[0].id, page1.rows[0].id);
        assert!(page2.has_more);

        let page2_cursor = content_document_list_repository::DocumentListPageCursor {
            created_at: page2.rows[0].created_at,
            document_id: page2.rows[0].id,
            text_value: page2.rows[0].projection_file_name.as_ref().map(|name| name.to_lowercase()),
            number_value: None,
            finished_value: None,
            value_is_null: false,
        };
        // Page 3 uses page 2's tuple. Last document, no more pages after it.
        let page3 = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs { cursor: Some(&page2_cursor), limit: 1, ..Default::default() },
        )
        .await
        .context("failed to fetch keyset-fallback page 3")?;
        assert_eq!(page3.rows.len(), 1);
        assert_eq!(page3.rows[0].id, document_ids[0], "page 3 must be zeta-visible.txt");
        assert!(!page3.has_more, "no fourth document should remain");

        let filename_search = content_document_list_repository::list_document_page_rows(
            &pool,
            content_document_list_repository::DocumentListPageQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                cursor: None,
                limit: 10,
                search: Some("alpha-visible"),
                sort: content_document_list_repository::DocumentListSortColumn::ExternalKey,
                sort_desc: false,
                normalized_search: None,
                search_statuses: &[],
                filters: &content_document_list_repository::DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                snapshot_at: chrono::Utc::now(),
                status_filter: &[],
                id_filter: None,
            },
        )
        .await
        .context("failed to search the visible filename")?;
        assert_eq!(filename_search.rows.len(), 1);
        assert_eq!(filename_search.rows[0].id, document_ids[2]);
        assert_eq!(filename_search.rows[0].external_key, "escaped%_external-key");

        let external_key_search = content_document_list_repository::list_document_page_rows(
            &pool,
            content_document_list_repository::DocumentListPageQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                cursor: None,
                limit: 10,
                search: Some("bbb-key"),
                sort: content_document_list_repository::DocumentListSortColumn::ExternalKey,
                sort_desc: false,
                normalized_search: None,
                search_statuses: &[],
                filters: &content_document_list_repository::DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                snapshot_at: chrono::Utc::now(),
                status_filter: &[],
                id_filter: None,
            },
        )
        .await
        .context("failed to search the external key")?;
        assert_eq!(external_key_search.rows.len(), 1);
        assert_eq!(external_key_search.rows[0].id, document_ids[1]);
        assert_eq!(external_key_search.rows[0].external_key, "bbb-key");

        let external_key_counts =
            content_document_list_repository::aggregate_document_list_status_counts(
                &pool,
                content_document_list_repository::DocumentListCountQuery {
                    library_id: fixture.library_id,
                    include_deleted: false,
                    search: Some("bbb-key"),
                    normalized_search: None,
                    search_statuses: &[],
                    filters: &content_document_list_repository::DocumentListFilters::default(),
                    search_ranges:
                        &content_document_list_repository::DocumentListSearchRanges::default(),
                    snapshot_at: chrono::Utc::now(),
                    status_filter: &[],
                    id_filter: None,
                },
            )
            .await?;
        assert_eq!(external_key_counts.total, Some(1));

        let literal_wildcard_search = content_document_list_repository::list_document_page_rows(
            &pool,
            content_document_list_repository::DocumentListPageQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                cursor: None,
                limit: 10,
                search: Some("%_"),
                sort: content_document_list_repository::DocumentListSortColumn::ExternalKey,
                sort_desc: false,
                normalized_search: None,
                search_statuses: &[],
                filters: &content_document_list_repository::DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                snapshot_at: chrono::Utc::now(),
                status_filter: &[],
                id_filter: None,
            },
        )
        .await
        .context("failed to execute literal wildcard search")?;
        assert_eq!(literal_wildcard_search.rows.len(), 1);
        assert_eq!(literal_wildcard_search.rows[0].id, document_ids[2]);
        assert_eq!(
            literal_wildcard_search.rows[0].external_key, "escaped%_external-key",
            "LIKE metacharacters in user input must be treated literally"
        );

        let literal_wildcard_counts =
            content_document_list_repository::aggregate_document_list_status_counts(
                &pool,
                content_document_list_repository::DocumentListCountQuery {
                    library_id: fixture.library_id,
                    include_deleted: false,
                    search: Some("%_"),
                    normalized_search: None,
                    search_statuses: &[],
                    filters: &content_document_list_repository::DocumentListFilters::default(),
                    search_ranges:
                        &content_document_list_repository::DocumentListSearchRanges::default(),
                    snapshot_at: chrono::Utc::now(),
                    status_filter: &[],
                    id_filter: None,
                },
            )
            .await?;
        assert_eq!(literal_wildcard_counts.total, Some(1));

        // A caller that resolved a document set of its own — the
        // `indexing=vector_gap` filter behind the dashboard's notice — asks for
        // exactly that set. An empty one is an empty page: the whole point of
        // the notice's destination is that it is narrow, and answering "nothing
        // matched" with the entire library is the defect it was added to fix.
        let empty_id_filter = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs { limit: 10, id_filter: Some(&[]), ..Default::default() },
        )
        .await
        .context("failed to execute the empty document-id filter")?;
        assert!(
            empty_id_filter.rows.is_empty(),
            "an explicitly empty id filter must not fall back to the whole library"
        );

        let single_id_filter = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs { limit: 10, id_filter: Some(&[document_ids[2]]), ..Default::default() },
        )
        .await
        .context("failed to execute the single document-id filter")?;
        assert_eq!(
            single_id_filter.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![document_ids[2]],
            "a non-empty id filter must select exactly the requested documents"
        );

        let searched_counts =
            content_document_list_repository::aggregate_document_list_status_counts(
                &pool,
                content_document_list_repository::DocumentListCountQuery {
                    library_id: fixture.library_id,
                    include_deleted: false,
                    search: Some("alpha-visible"),
                    normalized_search: None,
                    search_statuses: &[],
                    filters: &content_document_list_repository::DocumentListFilters::default(),
                    search_ranges:
                        &content_document_list_repository::DocumentListSearchRanges::default(),
                    snapshot_at: chrono::Utc::now(),
                    status_filter: &[],
                    id_filter: None,
                },
            )
            .await?;
        assert_eq!(searched_counts.total, Some(1));

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-107: a document admitted with no content — no `content_mutation` row,
/// no `ingest_job` row, no readable revision — must not be bucketed as
/// `queued`. `DERIVED_STATUS_CASE_SQL`'s final `else` used to hand out
/// `'queued'` for exactly this shape, misreporting permanently inert
/// documents as live work and inflating the library's in-flight count off a
/// queue depth of zero.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_with_no_mutation_and_no_job_is_bucketed_as_empty() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let document = content_document_repository::create_document(
            &pool,
            &NewContentDocument {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: "empty-bucket-key",
                document_state: "active",
                created_by_principal_id: Some(fixture.principal_id),
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .context("failed to create empty-bucket document")?;

        content_document_head_repository::upsert_document_head(
            &pool,
            &NewContentDocumentHead {
                publishing_mutation_item_id: None,
                document_id: document.id,
                active_revision_id: None,
                readable_revision_id: None,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to upsert empty-bucket document head")?;
        sqlx::query(
            "insert into knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role, created_at, updated_at, deleted_at
             ) values (
                $1, $2, $3, $4, $4, $4, 'active', null, null, 0,
                null, 'primary', now(), now(), null
             )",
        )
        .bind(document.id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind("empty-bucket-key")
        .execute(&pool)
        .await
        .context("failed to materialize empty-bucket knowledge projection")?;

        let counts = content_document_list_repository::aggregate_document_list_status_counts(
            &pool,
            content_document_list_repository::DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &content_document_list_repository::DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                snapshot_at: chrono::Utc::now(),
                status_filter: &[],
                id_filter: None,
            },
        )
        .await
        .context("failed to aggregate status counts for the empty-bucket document")?;

        assert_eq!(
            counts.queued.unwrap_or(0),
            0,
            "a document with no job and no mutation must not read as queued"
        );
        assert_eq!(
            counts.empty.unwrap_or(0),
            1,
            "a document with no job and no mutation must be bucketed as empty"
        );
        assert_eq!(counts.total.unwrap_or(0), 1);

        // The list surface uses the identical `DERIVED_STATUS_CASE_SQL` in its
        // `derived_status = any($status_filter)` predicate, so filtering by
        // `queued` must now exclude this document and filtering by `empty`
        // must include it.
        let queued_page = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs {
                limit: 10,
                sort: content_document_list_repository::DocumentListSortColumn::CreatedAt,
                status_filter: &["queued".to_string()],
                ..Default::default()
            },
        )
        .await
        .context("failed to list the queued bucket")?;
        assert!(
            queued_page.rows.iter().all(|row| row.id != document.id),
            "a document with no job and no mutation must not appear in the queued filter"
        );

        let empty_page = grid_page(
            &pool,
            fixture.library_id,
            Utc::now(),
            GridPageArgs {
                limit: 10,
                sort: content_document_list_repository::DocumentListSortColumn::CreatedAt,
                status_filter: &["empty".to_string()],
                ..Default::default()
            },
        )
        .await
        .context("failed to list the empty bucket")?;
        assert!(
            empty_page.rows.iter().any(|row| row.id == document.id),
            "a document with no job and no mutation must appear in the empty filter"
        );

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn operational_list_preserves_unpublished_deleted_and_hidden_documents() -> anyhow::Result<()>
{
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let visible = seed_grid_document(&pool, &fixture, "visible", "visible.txt", Some("text/plain"), 8).await?;
        let empty = seed_grid_document(&pool, &fixture, "empty", "mutable-empty-secret.txt", None, 0).await?;
        let hidden = seed_grid_document(&pool, &fixture, "hidden", "mutable-hidden-secret.txt", Some("text/plain"), 8).await?;
        let deleted = seed_grid_document(&pool, &fixture, "deleted", "deleted-secret.txt", Some("text/plain"), 8).await?;
        // An outstanding visibility floor hides the old publication without
        // discarding its operational document or retained raw head pointers.
        use ironrag_backend::infra::repositories::admission_repository::{VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint};
        admit_visibility_intent_with_failpoint(&pool, &VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id: hidden,
            requested_by_principal_id: Some(fixture.principal_id), request_surface: "internal".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: "grid-hidden-observation".to_owned(), source_identity: "grid-hidden-source".to_owned(), materialization_token: None,
                replacement: ironrag_contracts::documents::ReplacementIdentity {
                    checksum: format!("sha256:{}", "d".repeat(64)), byte_size: 8, mime_type: "text/plain".to_owned(),
                    file_name: "replacement.txt".to_owned(), title: None, language_code: None, source_uri: None, document_hint: None,
                },
            },
        }, None).await?;
        sqlx::query("update content_document set document_state = 'deleted', deleted_at = now() where id = $1")
            .bind(deleted).execute(&pool).await?;

        let filters = content_document_list_repository::DocumentListFilters::default();
        let ranges = content_document_list_repository::DocumentListSearchRanges::default();
        let snapshot = Utc::now();
        for include_deleted in [false, true] {
            let page = grid_page(&pool, fixture.library_id, snapshot, GridPageArgs { include_deleted, ..Default::default() }).await?;
            assert_eq!(page.rows.len(), if include_deleted { 4 } else { 3 });
            assert_eq!(page.rows.iter().any(|row| row.id == deleted), include_deleted);
            for row in &page.rows {
                if row.id == visible {
                    assert_eq!(row.projection_file_name.as_deref(), Some("visible.txt"));
                } else {
                    assert_eq!(row.projection_file_name, None);
                    assert_eq!(row.readable_revision_id, None);
                    assert_eq!(row.revision_mime_type, None);
                    assert_eq!(row.revision_byte_size, None);
                    assert_eq!(row.revision_title, None);
                }
            }
            let counts = content_document_list_repository::aggregate_document_list_status_counts(&pool,
                content_document_list_repository::DocumentListCountQuery {
                    library_id: fixture.library_id, include_deleted, search: None, normalized_search: None,
                    search_statuses: &[], filters: &filters, search_ranges: &ranges, snapshot_at: snapshot,
                    status_filter: &[], id_filter: None,
                }).await?;
            assert_eq!(counts.total, Some(page.rows.len() as i64));
            assert_eq!(counts.ready, Some(1));
            assert_eq!(counts.empty, Some(if include_deleted { 3 } else { 2 }));
        }
        for secret in ["mutable-empty-secret", "mutable-hidden-secret"] {
            assert!(grid_page(&pool, fixture.library_id, snapshot, GridPageArgs { search: Some(secret), ..Default::default() }).await?.rows.is_empty());
        }
        let null_names = content_document_list_repository::DocumentListFilters {
            file_name: content_document_list_repository::FilterValues { present: true, values: vec![], include_null: true },
            ..Default::default()
        };
        let null_page = grid_page_scoped(&pool, fixture.library_id, snapshot, &null_names, &ranges, GridPageArgs::default()).await?;
        assert_eq!(null_page.rows.len(), 2);
        assert!(null_page.rows.iter().any(|row| row.id == empty));
        assert!(null_page.rows.iter().any(|row| row.id == hidden));
        let facets = content_document_list_repository::list_document_facet_rows(&pool,
            content_document_list_repository::DocumentListFacetQuery {
                library_id: fixture.library_id, include_deleted: false,
                column: content_document_list_repository::DocumentFacetColumn::FileName,
                search: None, normalized_search: None, search_statuses: &[],
                facet_search: None, facet_search_statuses: &[], facet_search_range: None,
                filters: &filters, search_ranges: &ranges, status_filter: &[], snapshot_at: snapshot,
                cursor: None, limit: 10, id_filter: None,
            }).await?;
        assert_eq!(facets.rows.len(), 2);
        assert!(facets.rows.iter().any(|row| row.value_text.is_none() && row.facet_count == 2));
        assert!(facets.rows.iter().any(|row| row.value_text.as_deref() == Some("visible.txt") && row.facet_count == 1));
        // Both directions must traverse the non-null name and the full NULL
        // bucket once, using the same descending timestamp/id tie-breakers.
        for sort_desc in [false, true] {
            let mut seen = Vec::new();
            let mut cursor = None;
            for _ in 0..3 {
                let page = grid_page(&pool, fixture.library_id, snapshot, GridPageArgs { limit: 1, sort_desc, cursor: cursor.as_ref(), ..Default::default() }).await?;
                assert_eq!(page.rows.len(), 1);
                let row = &page.rows[0];
                assert!(!seen.contains(&row.id));
                seen.push(row.id);
                cursor = Some(content_document_list_repository::DocumentListPageCursor {
                    created_at: row.created_at, document_id: row.id,
                    text_value: row.projection_file_name.as_ref().map(|name| name.to_lowercase()),
                    number_value: None, finished_value: None, value_is_null: row.projection_file_name.is_none(),
                });
            }
            assert_eq!(seen[0], visible);
            assert!(!grid_page(&pool, fixture.library_id, snapshot, GridPageArgs { cursor: cursor.as_ref(), ..Default::default() }).await?.has_more);
            assert!(grid_page(&pool, fixture.library_id, snapshot, GridPageArgs { cursor: cursor.as_ref(), ..Default::default() }).await?.rows.is_empty());
        }
        let accepted = content_mutation_repository::create_mutation(&pool, &NewContentMutation {
            request_identity: &serde_json::json!({"target":{"kind":"existing","documentId":empty}}),
            workspace_id: fixture.workspace_id, library_id: fixture.library_id,
            operation_kind: "replace", requested_by_principal_id: Some(fixture.principal_id),
            request_surface: "internal", idempotency_key: None, source_identity: None,
            mutation_state: "accepted", failure_code: None, conflict_code: None,
        }).await?;
        sqlx::query("update content_document_head set latest_mutation_id = $2 where document_id = $1")
            .bind(empty).bind(accepted.id).execute(&pool).await?;
        let accepted_page = grid_page(&pool, fixture.library_id, snapshot, GridPageArgs {
            status_filter: &["processing".to_string()], ..Default::default()
        }).await?;
        assert_eq!(accepted_page.rows.len(), 1);
        assert_eq!(accepted_page.rows[0].id, empty);
        assert_eq!(accepted_page.rows[0].projection_file_name, None);
        assert_eq!(accepted_page.rows[0].revision_mime_type, None);
        anyhow::Ok(())
    }.await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn revision_admission_import_preserves_audit_only_latest_mutation() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let document_id = seed_grid_document(&pool, &fixture, "audit-import", "audit-import.txt", Some("text/plain"), 8).await?;
        let retained = Uuid::now_v7();
        let unrelated = Uuid::now_v7();
        // Model maintenance output directly: no live payload or invented attempt.
        let mut transaction = pool.begin().await?;
        for mutation_id in [retained, unrelated] {
            sqlx::query("insert into content_mutation_reference(id,workspace_id,library_id,request_surface) values($1,$2,$3,'internal')")
                .bind(mutation_id).bind(fixture.workspace_id).bind(fixture.library_id).execute(&mut *transaction).await?;
            sqlx::query("insert into content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,requested_at,predecessor_baseline_sha384,origin_workspace_id,origin_library_id)
                values($1,$2,$3,'replace','applied',now(),decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb','hex'),$2,$3)")
                .bind(mutation_id).bind(fixture.workspace_id).bind(fixture.library_id).execute(&mut *transaction).await?;
        }
        sqlx::query("update content_document_head set latest_mutation_id=$2 where document_id=$1")
            .bind(document_id).bind(retained).execute(&mut *transaction).await?;
        transaction.commit().await?;
        let head = content_document_head_repository::get_document_head(&pool, document_id).await?.context("import head")?;
        let input = NewContentDocumentHead {
            document_id, active_revision_id: head.active_revision_id, readable_revision_id: head.readable_revision_id,
            publishing_mutation_item_id: None, latest_mutation_id: Some(retained), latest_successful_attempt_id: None,
        };
        let preserved = content_document_head_repository::upsert_document_head(&pool, &input).await?;
        assert_eq!(preserved.latest_mutation_id, Some(retained));
        assert_eq!(preserved.readable_revision_id, head.readable_revision_id);
        assert_eq!(content_fingerprint_repository::get_current_revision_id(&pool, document_id).await?, head.readable_revision_id);
        assert!(content_document_head_repository::upsert_document_head(&pool, &NewContentDocumentHead {
            latest_mutation_id: Some(unrelated), ..input
        }).await.is_err(), "same-scope audit history is preservable, not transferable");
        let live: i64 = sqlx::query_scalar("select count(*) from content_mutation where id=any($1)")
            .bind(&[retained, unrelated][..]).fetch_one(&pool).await?;
        assert_eq!(live, 0, "audit-only history must not be repaired by fabricating payloads");
        anyhow::Ok(())
    }.await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn revision_admission_import_is_denied_by_intent_then_normal_replacement_survives_pruning()
-> anyhow::Result<()> {
    use ironrag_backend::infra::repositories::admission_repository::{
        ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
        VisibilityAdmissionRequest, admit_content_with_failpoint,
        admit_visibility_intent_with_failpoint,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let document_id = seed_grid_document(&pool, &fixture, "replacement-import", "import.txt", Some("text/plain"), 8).await?;
        let imported = content_document_head_repository::get_document_head(&pool, document_id).await?.context("import head")?
            .readable_revision_id.context("imported readable revision")?;
        let replacement = ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", hex::encode(Sha256::digest(b"replacement"))), byte_size: 11,
            mime_type: "text/plain".to_owned(), file_name: "replacement.txt".to_owned(),
            title: Some("Normal replacement".to_owned()), language_code: None, source_uri: None, document_hint: None,
        }.normalize()?;
        let intent = admit_visibility_intent_with_failpoint(&pool, &VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id,
            requested_by_principal_id: Some(fixture.principal_id), request_surface: "internal".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: "import-replacement-intent".to_owned(), source_identity: "replacement-source".to_owned(),
                materialization_token: None, replacement: replacement.clone(),
            },
        }, None).await?.into_bundle().receipt.visibility_intent_id;
        assert_eq!(content_fingerprint_repository::get_current_revision_id(&pool, document_id).await?, None);
        assert!(content_document_head_repository::document_has_unresolved_suppression(&pool, document_id).await?);
        assert!(content_document_head_repository::upsert_document_head(&pool, &NewContentDocumentHead {
            document_id, active_revision_id: Some(imported), readable_revision_id: Some(imported),
            publishing_mutation_item_id: None, latest_mutation_id: None, latest_successful_attempt_id: None,
        }).await.is_err(), "a certified import cannot discharge a later visibility obligation");
        assert_eq!(content_fingerprint_repository::get_current_revision_id(&pool, document_id).await?, None);
        let admitted = admit_content_with_failpoint(&pool, &ContentAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, operation_kind: "replace".to_owned(),
            requested_by_principal_id: Some(fixture.principal_id), request_surface: "internal".to_owned(),
            idempotency_key: Some("normal-after-import".to_owned()), source_identity: Some("replacement-source".to_owned()),
            target: ContentAdmissionTarget::Existing { document_id },
            revision: Some(RevisionAdmission {
                content_source_kind: "replace".to_owned(), replacement: replacement.clone(), visibility_intent_id: Some(intent),
                base_revision_id: None, storage_key: None,
            }), parent_async_operation_id: None, priority: TEST_INGEST_PRIORITY,
        }, None).await?.into_bundle();
        let revision_id = admitted.revision.context("replacement revision")?.id;
        let item_id = admitted.item.context("replacement item")?.id;
        let job_id = admitted.job.context("replacement job")?.id;
        let attempt = ingest_repository::create_ingest_attempt(&pool, &running_finalization_attempt(job_id)).await?;
        sqlx::query("update ingest_attempt set attempt_state='succeeded',finished_at=now() where id=$1")
            .bind(attempt.id).execute(&pool).await?;
        sqlx::query("update ingest_job set queue_state='completed',completed_at=now() where id=$1")
            .bind(job_id).execute(&pool).await?;
        sqlx::query("update content_mutation set mutation_state='applied',completed_at=now() where id=$1")
            .bind(admitted.mutation.id).execute(&pool).await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
            .bind(item_id).execute(&pool).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now(),vector_state='ready',vector_ready_at=now(),graph_state='ready',graph_ready_at=now() where revision_id=$1")
            .bind(revision_id).execute(&pool).await?;
        content_document_head_repository::upsert_document_head(&pool, &NewContentDocumentHead {
            document_id, active_revision_id: Some(revision_id), readable_revision_id: Some(revision_id),
            publishing_mutation_item_id: Some(item_id), latest_mutation_id: Some(admitted.mutation.id), latest_successful_attempt_id: Some(attempt.id),
        }).await.context("publish normal intent-bound replacement")?;
        assert_eq!(content_fingerprint_repository::get_current_revision_id(&pool, document_id).await?, Some(revision_id));
        sqlx::query("delete from ingest_job where id=$1").bind(job_id).execute(&pool).await?;
        sqlx::query("delete from content_mutation_item where id=$1").bind(item_id).execute(&pool).await?;
        assert!(ingest_repository::get_ingest_attempt_by_id(&pool, attempt.id).await?.is_none());
        assert_eq!(content_fingerprint_repository::get_current_revision_id(&pool, document_id).await?, Some(revision_id), "retention cannot revoke admitted readable bytes");
        assert_eq!(content_revision_repository::get_revision_replacement_identity(&pool, revision_id).await?, Some(replacement));
        assert!(!content_document_head_repository::document_has_unresolved_suppression(&pool, document_id).await?);
        let preserved = content_document_head_repository::upsert_document_head(&pool, &NewContentDocumentHead {
            document_id, active_revision_id: Some(revision_id), readable_revision_id: Some(revision_id),
            publishing_mutation_item_id: None, latest_mutation_id: Some(admitted.mutation.id), latest_successful_attempt_id: None,
        }).await.context("preserve replacement after operational-history pruning")?;
        assert_eq!(preserved.readable_revision_id, Some(revision_id));
        anyhow::Ok(())
    }.await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

async fn repository_readable_head(
    pool: &sqlx::PgPool,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Uuid,
    attempt_id: Uuid,
) -> anyhow::Result<NewContentDocumentHead> {
    anyhow::ensure!(
        content_revision_repository::get_revision_by_id(pool, revision_id).await?.is_some()
    );
    Ok(NewContentDocumentHead {
        document_id,
        publishing_mutation_item_id: None,
        active_revision_id: Some(revision_id),
        readable_revision_id: Some(revision_id),
        latest_mutation_id: Some(mutation_id),
        latest_successful_attempt_id: Some(attempt_id),
    })
}

fn running_finalization_attempt(job_id: Uuid) -> NewIngestAttempt {
    NewIngestAttempt {
        job_id,
        attempt_number: 1,
        worker_principal_id: None,
        lease_token: None,
        knowledge_generation_id: None,
        attempt_state: "running".to_owned(),
        current_stage: Some("finalizing".to_owned()),
        started_at: None,
        heartbeat_at: None,
        finished_at: None,
        failure_class: None,
        failure_code: None,
        failure_message: None,
        progress_percent: 90,
        retryable: false,
    }
}

fn replacement_mutation_identity(
    revision: &content_revision_repository::ContentRevisionRow,
    base_revision_id: Option<Uuid>,
    source_identity: Option<&str>,
    file_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "workspaceId":revision.workspace_id,"libraryId":revision.library_id,"operationKind":"replace",
        "sourceIdentity":source_identity,"target":{"kind":"existing","documentId":revision.document_id},
        "revision":{"contentSourceKind":revision.content_source_kind,"baseRevisionId":base_revision_id,"visibilityIntentId":null},
        "replacement":{"checksum":revision.checksum,"byteSize":revision.byte_size,"mimeType":revision.mime_type,"fileName":file_name,
            "title":revision.title,"languageCode":revision.language_code,"sourceUri":revision.source_uri,"documentHint":revision.document_hint},
    })
}

/// Seeds one data-grid document. `mime_type` `None` admits a document with no
/// revision — the renderable-but-`empty` shape.
async fn seed_grid_document(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    key: &str,
    file_name: &str,
    mime_type: Option<&str>,
    byte_size: i64,
) -> anyhow::Result<Uuid> {
    let document = content_document_repository::create_document(
        pool,
        &NewContentDocument {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            external_key: key,
            document_state: "active",
            created_by_principal_id: Some(fixture.principal_id),
            parent_external_key: None,
            parent_document_id: None,
            document_role: "primary",
        },
    )
    .await
    .context("failed to create grid document")?;
    let revision_id = match mime_type {
        Some(mime_type) => {
            let revision = create_imported_fixture_revision(
                pool,
                &NewContentRevision {
                    admission_identity: None,
                    document_id: document.id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    revision_number: 1,
                    parent_revision_id: None,
                    content_source_kind: "upload",
                    checksum: &format!("sha256:{}", hex::encode(Sha256::digest(key.as_bytes()))),
                    mime_type,
                    byte_size,
                    title: Some(file_name),
                    language_code: Some("en"),
                    source_uri: None,
                    document_hint: None,
                    storage_key: Some(key),
                    created_by_principal_id: Some(fixture.principal_id),
                },
                file_name,
            )
            .await
            .context("failed to create grid revision")?;
            Some(revision.id)
        }
        None => None,
    };
    content_document_head_repository::upsert_document_head(
        pool,
        &NewContentDocumentHead {
            document_id: document.id,
            active_revision_id: revision_id,
            readable_revision_id: revision_id,
            publishing_mutation_item_id: None,
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await
    .context("failed to upsert grid document head")?;
    sqlx::query(
        "insert into knowledge_document (
            document_id, workspace_id, library_id, external_key, file_name, title,
            document_state, active_revision_id, readable_revision_id, latest_revision_no,
            parent_document_id, document_role, created_at, updated_at, deleted_at
         ) values (
            $1, $2, $3, $4, $5, $5, 'active', $6, $6, $7,
            null, 'primary', now(), now(), null
         )",
    )
    .bind(document.id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(key)
    .bind(file_name)
    .bind(revision_id)
    .bind(if revision_id.is_some() { 1_i32 } else { 0_i32 })
    .execute(pool)
    .await
    .context("failed to materialize grid knowledge projection")?;
    if let Some(revision_id) = revision_id {
        seed_grid_readable_revision(pool, revision_id).await?;
    }
    Ok(document.id)
}

async fn seed_grid_readable_revision(pool: &PgPool, revision_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "insert into knowledge_revision (
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint,
            mime_type, checksum, title, byte_size, normalized_text, text_checksum,
            text_state, vector_state, graph_state, text_readable_at,
            vector_ready_at, graph_ready_at, created_at
         ) select id, workspace_id, library_id, document_id, revision_number,
            'ready', content_source_kind::text, storage_key, source_uri, document_hint,
            mime_type, checksum, title, byte_size, 'grid fixture', checksum,
            'text_readable', 'ready', 'ready', now(), now(), now(), created_at
           from content_revision where id = $1",
    )
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Arguments for a document-list page walk. `Default` is the common
/// external-key ascending, first-page, unfiltered read.
struct GridPageArgs<'a> {
    include_deleted: bool,
    cursor: Option<&'a content_document_list_repository::DocumentListPageCursor>,
    limit: u32,
    search: Option<&'a str>,
    sort: content_document_list_repository::DocumentListSortColumn,
    sort_desc: bool,
    status_filter: &'a [String],
    id_filter: Option<&'a [Uuid]>,
}

impl Default for GridPageArgs<'_> {
    fn default() -> Self {
        Self {
            include_deleted: false,
            cursor: None,
            limit: 50,
            search: None,
            sort: content_document_list_repository::DocumentListSortColumn::ExternalKey,
            sort_desc: false,
            status_filter: &[],
            id_filter: None,
        }
    }
}

/// The one place the integration tests build a `DocumentListPageQuery`.
async fn grid_page_scoped(
    pool: &PgPool,
    library_id: Uuid,
    snapshot_at: chrono::DateTime<Utc>,
    filters: &content_document_list_repository::DocumentListFilters,
    search_ranges: &content_document_list_repository::DocumentListSearchRanges,
    args: GridPageArgs<'_>,
) -> anyhow::Result<content_document_list_repository::ContentDocumentListPage> {
    content_document_list_repository::list_document_page_rows(
        pool,
        content_document_list_repository::DocumentListPageQuery {
            library_id,
            include_deleted: args.include_deleted,
            cursor: args.cursor,
            limit: args.limit,
            search: args.search,
            normalized_search: None,
            search_statuses: &[],
            filters,
            search_ranges,
            snapshot_at,
            sort: args.sort,
            sort_desc: args.sort_desc,
            status_filter: args.status_filter,
            id_filter: args.id_filter,
        },
    )
    .await
    .map_err(Into::into)
}

async fn grid_page(
    pool: &PgPool,
    library_id: Uuid,
    snapshot_at: chrono::DateTime<Utc>,
    args: GridPageArgs<'_>,
) -> anyhow::Result<content_document_list_repository::ContentDocumentListPage> {
    let filters = content_document_list_repository::DocumentListFilters::default();
    let search_ranges = content_document_list_repository::DocumentListSearchRanges::default();
    grid_page_scoped(pool, library_id, snapshot_at, &filters, &search_ranges, args).await
}

/// The consolidated list read's scope facts for one filter scope: rollup
/// health, the whole-scope NULL-cost probe and the row/status counts, read in
/// the same REPEATABLE READ snapshot the page would be. Tests use it where
/// they do not need the rows.
async fn grid_scope_read(
    pool: &PgPool,
    query: content_document_list_repository::DocumentListCountQuery<'_>,
) -> anyhow::Result<content_document_list_repository::DocumentListScopeRead> {
    let page_query = content_document_list_repository::DocumentListPageQuery {
        library_id: query.library_id,
        include_deleted: query.include_deleted,
        cursor: None,
        limit: 1,
        search: query.search,
        normalized_search: query.normalized_search,
        search_statuses: query.search_statuses,
        filters: query.filters,
        search_ranges: query.search_ranges,
        snapshot_at: query.snapshot_at,
        sort: content_document_list_repository::DocumentListSortColumn::CreatedAt,
        sort_desc: true,
        status_filter: query.status_filter,
        id_filter: query.id_filter,
    };
    Ok(content_document_list_repository::read_document_list_page(pool, page_query, query)
        .await?
        .scope)
}

async fn grid_list_ids(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot_at: chrono::DateTime<chrono::Utc>,
    search: Option<&str>,
    filters: &content_document_list_repository::DocumentListFilters,
) -> anyhow::Result<Vec<Uuid>> {
    let search_ranges = content_document_list_repository::DocumentListSearchRanges::default();
    let page = grid_page_scoped(
        pool,
        fixture.library_id,
        snapshot_at,
        filters,
        &search_ranges,
        GridPageArgs { search, ..Default::default() },
    )
    .await?;
    Ok(page.rows.into_iter().map(|row| row.id).collect())
}

/// Query dimensions shared by every facet page read: the faceted column, the
/// filters applied to the shared base CTE, the keyset cursor and the page size.
struct GridFacetQuery<'a> {
    column: content_document_list_repository::DocumentFacetColumn,
    filters: &'a content_document_list_repository::DocumentListFilters,
    cursor: Option<&'a content_document_list_repository::DocumentFacetCursorValue>,
    limit: u32,
}

async fn grid_facet_page(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot_at: chrono::DateTime<chrono::Utc>,
    query: GridFacetQuery<'_>,
) -> anyhow::Result<content_document_list_repository::DocumentFacetPage> {
    content_document_list_repository::list_document_facet_rows(
        pool,
        content_document_list_repository::DocumentListFacetQuery {
            library_id: fixture.library_id,
            status_filter: &[],
            include_deleted: false,
            column: query.column,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            facet_search: None,
            facet_search_statuses: &[],
            facet_search_range: None,
            filters: query.filters,
            search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(),
            snapshot_at,
            cursor: query.cursor,
            limit: query.limit,
            id_filter: None,
        },
    )
    .await
    .map_err(Into::into)
}

fn grid_facet_text_counts(
    page: &content_document_list_repository::DocumentFacetPage,
) -> Vec<(String, i64)> {
    let mut counts: Vec<(String, i64)> = page
        .rows
        .iter()
        .map(|row| (row.value_text.clone().unwrap_or_default(), row.facet_count))
        .collect();
    counts.sort();
    counts
}

async fn seed_grid_attempt(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    document_id: Uuid,
    started_at: chrono::DateTime<Utc>,
    finished_at: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<Uuid> {
    let job = ingest_repository::create_ingest_job(
        pool,
        &NewIngestJob {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(document_id),
            knowledge_revision_id: None,
            job_kind: "content_mutation".to_owned(),
            queue_state: if finished_at.is_some() { "completed" } else { "leased" }.to_owned(),
            priority: TEST_INGEST_PRIORITY,
            dedupe_key: None,
            queued_at: Some(started_at),
            available_at: Some(started_at),
            completed_at: finished_at,
        },
    )
    .await?;
    let attempt_id = sqlx::query_scalar::<_, Uuid>(
        "insert into ingest_attempt (job_id, attempt_number, attempt_state, started_at, finished_at)
         values ($1, 1, case when $3::timestamptz is null then 'running'::ingest_attempt_state
                       else 'succeeded'::ingest_attempt_state end, $2, $3)
         returning id",
    )
    .bind(job.id)
    .bind(started_at)
    .bind(finished_at)
    .fetch_one(pool)
    .await?;
    Ok(attempt_id)
}

/// Seed inputs for a ready grid document with one completed attempt.
struct GridAttemptSeed<'a> {
    key: &'a str,
    file_name: &'a str,
    mime_type: &'a str,
    byte_size: i64,
    started_at: chrono::DateTime<Utc>,
    duration_ms: i64,
}

/// Seeds a ready grid document whose `created_at` and attempt start share
/// `started_at`, with the attempt finishing `duration_ms` later.
async fn seed_grid_dated_attempt(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    seed: GridAttemptSeed<'_>,
) -> anyhow::Result<Uuid> {
    let id = seed_grid_document(
        pool,
        fixture,
        seed.key,
        seed.file_name,
        Some(seed.mime_type),
        seed.byte_size,
    )
    .await?;
    sqlx::query("update content_document set created_at = $2 where id = $1")
        .bind(id)
        .bind(seed.started_at)
        .execute(pool)
        .await?;
    seed_grid_attempt(
        pool,
        fixture,
        id,
        seed.started_at,
        Some(seed.started_at + chrono::Duration::milliseconds(seed.duration_ms)),
    )
    .await?;
    Ok(id)
}

async fn collect_grid_sort_pages(
    pool: &PgPool,
    library_id: Uuid,
    sort: content_document_list_repository::DocumentListSortColumn,
    descending: bool,
    snapshot_at: chrono::DateTime<Utc>,
) -> anyhow::Result<Vec<Uuid>> {
    use content_document_list_repository::{
        DocumentListFilters, DocumentListPageCursor, DocumentListSearchRanges,
    };
    let filters = DocumentListFilters::default();
    let search_ranges = DocumentListSearchRanges::default();
    let mut cursor = None;
    let mut ids = Vec::new();
    // The explicit bound also fails if a broken cursor repeats a page forever.
    for _ in 0..10 {
        let page = grid_page_scoped(
            pool,
            library_id,
            snapshot_at,
            &filters,
            &search_ranges,
            GridPageArgs {
                cursor: cursor.as_ref(),
                limit: 1,
                sort,
                sort_desc: descending,
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(page.rows.len(), 1, "each seeded sort page has one row");
        let row = &page.rows[0];
        assert!(!ids.contains(&row.id), "cursor repeated a document");
        ids.push(row.id);
        if !page.has_more {
            return Ok(ids);
        }
        let (text_value, number_value, finished_value, value_is_null) = match sort {
            content_document_list_repository::DocumentListSortColumn::Cost => {
                (row.cost_total.map(|cost| cost.to_string()), None, None, row.cost_total.is_none())
            }
            content_document_list_repository::DocumentListSortColumn::PipelineTime => {
                (None, row.pipeline_time_ms, None, row.pipeline_time_ms.is_none())
            }
            content_document_list_repository::DocumentListSortColumn::FinishedAt => {
                (None, None, row.finished_at, row.finished_at.is_none())
            }
            _ => unreachable!("this helper exercises the three grid sorts"),
        };
        cursor = Some(DocumentListPageCursor {
            created_at: row.created_at,
            document_id: row.id,
            text_value,
            number_value,
            finished_value,
            value_is_null,
        });
    }
    anyhow::bail!("document keyset did not finish within the seeded row bound")
}

/// Seeds the accounting-sorts fixture: three cost-tracked documents whose
/// attempts finish at distinct offsets from `snapshot`, plus one document whose
/// mixed-currency rollup is NULL. Returns the populated `(id, milliseconds)`
/// rows and the NULL document id.
async fn seed_accounting_sorts_fixture(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot: chrono::DateTime<Utc>,
) -> anyhow::Result<(Vec<(Uuid, i64)>, Uuid)> {
    let finish = snapshot - chrono::Duration::minutes(1);
    let mut populated = Vec::new();
    for (index, milliseconds) in [2000_i64, 1000, 2000].into_iter().enumerate() {
        let id = seed_grid_document(
            pool,
            fixture,
            &format!("cost-grid-{index}"),
            &format!("cost-grid-{index}.txt"),
            Some("text/plain"),
            10,
        )
        .await?;
        // Equal created_at forces the UUID tie-breaker across one-row pages.
        sqlx::query("update content_document set created_at = $2 where id = $1")
            .bind(id)
            .bind(snapshot)
            .execute(pool)
            .await?;
        let completed_at = finish + chrono::Duration::milliseconds(milliseconds);
        let job_id = seed_grid_attempt(pool, fixture, id, finish, Some(completed_at)).await?;
        sqlx::query(
            "insert into billing_execution_cost
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              knowledge_document_id, total_cost, currency_code, provider_call_count,
              unpriced_provider_call_count)
             values ('ingest_attempt', $1, $2, $3, $4, $5, 'USD', 1, 0)",
        )
        .bind(job_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(id)
        .bind(rust_decimal::Decimal::new(milliseconds, 3))
        .execute(pool)
        .await?;
        populated.push((id, milliseconds));
    }
    let missing =
        seed_grid_document(pool, fixture, "cost-grid-null", "cost-grid-null", None, 0).await?;
    // A mixed currency rollup is NULL, not a fabricated sortable sum.
    for currency in ["USD", "EUR"] {
        sqlx::query(
            "insert into billing_execution_cost
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              knowledge_document_id, total_cost, currency_code, provider_call_count,
              unpriced_provider_call_count)
             values ('ingest_attempt', $1, $2, $3, $4, 1, $5, 1, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(missing)
        .bind(currency)
        .execute(pool)
        .await?;
    }
    Ok((populated, missing))
}

/// Asserts the cost, pipeline-time and finished-at keysets page in NULL-last,
/// stable (UUID-tie-broken) order in both directions.
async fn assert_accounting_sort_orders(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot: chrono::DateTime<Utc>,
    populated: &[(Uuid, i64)],
    missing: Uuid,
) -> anyhow::Result<()> {
    use content_document_list_repository::DocumentListSortColumn;
    for sort in [
        DocumentListSortColumn::Cost,
        DocumentListSortColumn::PipelineTime,
        DocumentListSortColumn::FinishedAt,
    ] {
        for descending in [false, true] {
            let mut expected = populated.to_vec();
            expected.sort_by(|(left_id, left_value), (right_id, right_value)| {
                let primary = if descending {
                    right_value.cmp(left_value)
                } else {
                    left_value.cmp(right_value)
                };
                primary.then_with(|| right_id.cmp(left_id))
            });
            let mut expected_ids: Vec<_> = expected.iter().map(|(id, _)| *id).collect();
            expected_ids.push(missing);
            assert_eq!(
                collect_grid_sort_pages(pool, fixture.library_id, sort, descending, snapshot)
                    .await?,
                expected_ids,
                "NULL-last stable cursor order for {sort:?}, descending={descending}",
            );
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_grid_accounting_sorts_keep_nulls_last_and_stable_keysets() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let (populated, missing) = seed_accounting_sorts_fixture(&pool, &fixture, snapshot).await?;
        assert_accounting_sort_orders(&pool, &fixture, snapshot, &populated, missing).await?;
        for column in [
            content_document_list_repository::DocumentFacetColumn::Cost,
            content_document_list_repository::DocumentFacetColumn::PipelineTime,
            content_document_list_repository::DocumentFacetColumn::FinishedAt,
        ] {
            use content_document_list_repository::{DocumentFacetCursorValue, DocumentListFilters};
            let mut cursor = None;
            let mut counts = Vec::new();
            for _ in 0..4 {
                let page = grid_facet_page(
                    &pool,
                    &fixture,
                    snapshot,
                    GridFacetQuery {
                        column,
                        filters: &DocumentListFilters::default(),
                        cursor: cursor.as_ref(),
                        limit: 1,
                    },
                )
                .await?;
                assert_eq!(page.rows.len(), 1);
                let row = &page.rows[0];
                counts.push(row.facet_count);
                cursor = Some(if let Some(decimal) = row.value_decimal {
                    DocumentFacetCursorValue::Decimal(decimal)
                } else if let Some(number) = row.value_number {
                    DocumentFacetCursorValue::Number(number)
                } else if let Some(timestamp) = row.value_timestamp {
                    DocumentFacetCursorValue::Timestamp(timestamp)
                } else {
                    DocumentFacetCursorValue::Null
                });
                if !page.has_more {
                    assert!(matches!(cursor, Some(DocumentFacetCursorValue::Null)));
                    break;
                }
            }
            assert_eq!(counts, vec![1, 2, 1], "numeric/timestamp/decimal facets count full groups");
            let after_null = grid_facet_page(
                &pool,
                &fixture,
                snapshot,
                GridFacetQuery {
                    column,
                    filters: &DocumentListFilters::default(),
                    cursor: cursor.as_ref(),
                    limit: 1,
                },
            )
            .await?;
            assert!(after_null.rows.is_empty());
            assert!(!after_null.has_more);
        }

        let processing = seed_grid_document(
            &pool,
            &fixture,
            "live-duration",
            "live-duration.pdf",
            Some("application/pdf"),
            10,
        )
        .await?;
        seed_grid_attempt(
            &pool,
            &fixture,
            processing,
            snapshot - chrono::Duration::milliseconds(45_000),
            None,
        )
        .await?;
        let duration_filter = content_document_list_repository::DocumentListFilters {
            pipeline_time: content_document_list_repository::FilterValues {
                present: true,
                include_null: false,
                values: vec![45_000],
            },
            ..Default::default()
        };
        assert_eq!(
            grid_list_ids(&pool, &fixture, snapshot, None, &duration_filter).await?,
            vec![processing],
            "a facet's snapshot must be reusable for selecting its live duration",
        );
        assert!(
            grid_list_ids(
                &pool,
                &fixture,
                snapshot + chrono::Duration::seconds(1),
                None,
                &duration_filter,
            )
            .await?
            .is_empty(),
            "changing the clock would invalidate the selected duration"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-225: literal search over every canonical value, strictly typed
/// per-column filters, and the facet endpoint's self-exclusion, keyset paging
/// and count agreement over the shared base CTE.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_page_typed_filters_search_and_facets() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentFacetColumn, DocumentFacetCursorValue, DocumentListFilters, FilterValues,
    };

    let settings =
        Settings::from_env().context("failed to load settings for content repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;

    let result = async {
        let pdf = seed_grid_document(
            &pool,
            &fixture,
            "grid-pdf",
            "Report.pdf",
            Some("application/pdf"),
            512,
        )
        .await?;
        let txt =
            seed_grid_document(&pool, &fixture, "grid-txt", "notes.txt", Some("text/plain"), 64)
                .await?;
        let empty =
            seed_grid_document(&pool, &fixture, "grid-empty", "orphan.bin", None, 0).await?;
        let snapshot_at = Utc::now();

        // Expanded literal search reaches the canonical MIME value, not just
        // the external key / display name.
        let mime_search = grid_list_ids(
            &pool,
            &fixture,
            snapshot_at,
            Some("application/pdf"),
            &DocumentListFilters::default(),
        )
        .await?;
        assert_eq!(mime_search, vec![pdf], "search must match the canonical MIME value");

        let matching_names = content_document_list_repository::list_document_facet_rows(
            &pool,
            content_document_list_repository::DocumentListFacetQuery {
                library_id: fixture.library_id,
                status_filter: &[],
                include_deleted: false,
                column: DocumentFacetColumn::FileName,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                facet_search: Some("REPORT"),
                facet_search_statuses: &[],
                facet_search_range: None,
                filters: &DocumentListFilters::default(),
                search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(
                ),
                snapshot_at,
                cursor: None,
                limit: 50,
                id_filter: None,
            },
        )
        .await?;
        assert_eq!(matching_names.rows.len(), 1, "facet search is case-insensitive");
        assert_eq!(matching_names.rows[0].value_text.as_deref(), Some("Report.pdf"));

        // Typed status filter narrows to the ready bucket.
        let ready_filter = DocumentListFilters {
            status: FilterValues {
                present: true,
                include_null: false,
                values: vec!["ready".to_string()],
            },
            ..Default::default()
        };
        let ready_ids = grid_list_ids(&pool, &fixture, snapshot_at, None, &ready_filter).await?;
        assert!(ready_ids.contains(&pdf) && ready_ids.contains(&txt));
        assert!(!ready_ids.contains(&empty), "the empty document is not ready");

        // A NULL-bucket filter selects the document with no revision size.
        let null_size = DocumentListFilters {
            file_size: FilterValues { present: true, include_null: true, values: vec![] },
            ..Default::default()
        };
        let null_ids = grid_list_ids(&pool, &fixture, snapshot_at, None, &null_size).await?;
        assert_eq!(null_ids, vec![empty], "fileSize [null] selects the NULL bucket");

        // Facet counts over the same base CTE.
        let status_page = grid_facet_page(
            &pool,
            &fixture,
            snapshot_at,
            GridFacetQuery {
                column: DocumentFacetColumn::Status,
                filters: &DocumentListFilters::default(),
                cursor: None,
                limit: 10,
            },
        )
        .await?;
        assert_eq!(
            grid_facet_text_counts(&status_page),
            vec![("empty".to_string(), 1), ("ready".to_string(), 2)]
        );

        for (statuses, ids, expected) in [
            (vec!["ready".to_owned()], Some(vec![txt]), 1_i64),
            (vec!["failed".to_owned()], None, 0_i64),
            (Vec::new(), Some(Vec::new()), 0_i64),
        ] {
            let filters = DocumentListFilters::default();
            let counts = content_document_list_repository::aggregate_document_list_status_counts(
                &pool,
                content_document_list_repository::DocumentListCountQuery {
                    library_id: fixture.library_id,
                    include_deleted: false,
                    search: None,
                    normalized_search: None,
                    search_statuses: &[],
                    filters: &filters,
                    search_ranges:
                        &content_document_list_repository::DocumentListSearchRanges::default(),
                    snapshot_at,
                    status_filter: &statuses,
                    id_filter: ids.as_deref(),
                },
            )
            .await?;
            assert_eq!(
                counts.total,
                Some(expected),
                "total honors pill and resolved document scope"
            );
            let facets = content_document_list_repository::list_document_facet_rows(
                &pool,
                content_document_list_repository::DocumentListFacetQuery {
                    library_id: fixture.library_id,
                    include_deleted: false,
                    column: DocumentFacetColumn::Status,
                    search: None,
                    normalized_search: None,
                    search_statuses: &[],
                    facet_search: None,
                    facet_search_statuses: &[],
                    facet_search_range: None,
                    filters: &filters,
                    search_ranges:
                        &content_document_list_repository::DocumentListSearchRanges::default(),
                    snapshot_at,
                    cursor: None,
                    limit: 50,
                    status_filter: &statuses,
                    id_filter: ids.as_deref(),
                },
            )
            .await?;
            assert_eq!(
                facets.rows.iter().map(|row| row.facet_count).sum::<i64>(),
                expected,
                "facet self-exclusion must not discard pill or resolved ids"
            );
        }

        // Self-exclusion: faceting status while filtering status still returns
        // every status bucket, because the selected column's own filter is
        // dropped.
        let self_excluded = grid_facet_page(
            &pool,
            &fixture,
            snapshot_at,
            GridFacetQuery {
                column: DocumentFacetColumn::Status,
                filters: &ready_filter,
                cursor: None,
                limit: 10,
            },
        )
        .await?;
        assert_eq!(
            grid_facet_text_counts(&self_excluded),
            vec![("empty".to_string(), 1), ("ready".to_string(), 2)],
            "the facet must exclude its own column's filter"
        );

        // Keyset paging across facet values, no duplicates.
        let first = grid_facet_page(
            &pool,
            &fixture,
            snapshot_at,
            GridFacetQuery {
                column: DocumentFacetColumn::Status,
                filters: &DocumentListFilters::default(),
                cursor: None,
                limit: 1,
            },
        )
        .await?;
        assert!(first.has_more);
        assert_eq!(grid_facet_text_counts(&first), vec![("empty".to_string(), 1)]);
        let cursor = DocumentFacetCursorValue::Text("empty".to_string());
        let second = grid_facet_page(
            &pool,
            &fixture,
            snapshot_at,
            GridFacetQuery {
                column: DocumentFacetColumn::Status,
                filters: &DocumentListFilters::default(),
                cursor: Some(&cursor),
                limit: 1,
            },
        )
        .await?;
        assert!(!second.has_more);
        assert_eq!(grid_facet_text_counts(&second), vec![("ready".to_string(), 2)]);

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

async fn grid_list_ids_with_ranges(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot_at: chrono::DateTime<Utc>,
    search_ranges: &content_document_list_repository::DocumentListSearchRanges,
) -> anyhow::Result<Vec<Uuid>> {
    let filters = content_document_list_repository::DocumentListFilters::default();
    let page = grid_page_scoped(
        pool,
        fixture.library_id,
        snapshot_at,
        &filters,
        search_ranges,
        GridPageArgs::default(),
    )
    .await?;
    Ok(page.rows.into_iter().map(|row| row.id).collect())
}

async fn grid_facet_page_with_ranges(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot_at: chrono::DateTime<Utc>,
    column: content_document_list_repository::DocumentFacetColumn,
    search_ranges: &content_document_list_repository::DocumentListSearchRanges,
    limit: u32,
) -> anyhow::Result<content_document_list_repository::DocumentFacetPage> {
    let filters = content_document_list_repository::DocumentListFilters::default();
    content_document_list_repository::list_document_facet_rows(
        pool,
        content_document_list_repository::DocumentListFacetQuery {
            library_id: fixture.library_id,
            include_deleted: false,
            column,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            facet_search: None,
            facet_search_statuses: &[],
            facet_search_range: None,
            filters: &filters,
            search_ranges,
            status_filter: &[],
            snapshot_at,
            cursor: None,
            limit,
            id_filter: None,
        },
    )
    .await
    .map_err(Into::into)
}

async fn seed_grid_cost(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    job_id: Uuid,
    document_id: Uuid,
    currency: &str,
    total: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into billing_execution_cost
         (owning_execution_kind, owning_execution_id, workspace_id, library_id,
          knowledge_document_id, total_cost, currency_code, provider_call_count,
          unpriced_provider_call_count)
         values ('ingest_attempt', $1, $2, $3, $4, $5, $6, 1, 0)",
    )
    .bind(job_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(document_id)
    .bind(rust_decimal::Decimal::from(total))
    .bind(currency)
    .execute(pool)
    .await?;
    Ok(())
}

/// IRG-225: `searchRanges` are OR alternatives to the text/status search, and
/// the duration/size projections match the displayed grid values at one pinned
/// snapshot.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_page_search_ranges_match_displayed_size_duration_and_dates() -> anyhow::Result<()>
{
    use content_document_list_repository::{
        DocumentFacetColumn, DocumentListFilters, DocumentListSearchRanges,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let finish = snapshot - chrono::Duration::minutes(5);
        let a = seed_grid_dated_attempt(
            &pool,
            &fixture,
            GridAttemptSeed {
                key: "range-a",
                file_name: "range-a.pdf",
                mime_type: "application/pdf",
                byte_size: 1537,
                started_at: finish,
                duration_ms: 90125,
            },
        )
        .await?;
        let _b = seed_grid_dated_attempt(
            &pool,
            &fixture,
            GridAttemptSeed {
                key: "range-b",
                file_name: "range-b.txt",
                mime_type: "text/plain",
                byte_size: 64,
                started_at: finish - chrono::Duration::hours(1),
                duration_ms: 500,
            },
        )
        .await?;

        // Size range selects the 1537-byte document only (half-open [1485,1588)).
        let size_ranges =
            DocumentListSearchRanges { file_size: Some((1485, 1588)), ..Default::default() };
        assert_eq!(
            grid_list_ids_with_ranges(&pool, &fixture, snapshot, &size_ranges).await?,
            vec![a],
            "size range must match the displayed byte size"
        );

        // Duration range selects the 90125 ms document only.
        let time_ranges =
            DocumentListSearchRanges { pipeline_time: Some((90000, 91000)), ..Default::default() };
        assert_eq!(
            grid_list_ids_with_ranges(&pool, &fixture, snapshot, &time_ranges).await?,
            vec![a],
            "duration range must match the displayed millisecond duration"
        );

        // Date range matches the upload instant OR the displayed finish.
        let date_ranges = DocumentListSearchRanges {
            date: Some((
                finish - chrono::Duration::seconds(10),
                finish + chrono::Duration::minutes(3),
            )),
            ..Default::default()
        };
        assert_eq!(
            grid_list_ids_with_ranges(&pool, &fixture, snapshot, &date_ranges).await?,
            vec![a],
            "date range must match uploaded_at or the displayed finish"
        );

        // count honors the same OR-hint search set.
        let empty_filters = DocumentListFilters::default();
        let counts = content_document_list_repository::aggregate_document_list_status_counts(
            &pool,
            content_document_list_repository::DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &empty_filters,
                search_ranges: &time_ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?;
        assert_eq!(counts.total, Some(1), "counts must reflect the range-narrowed set");

        // The status facet under the same range narrows identically.
        let facets = grid_facet_page_with_ranges(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Status,
            &time_ranges,
            50,
        )
        .await?;
        assert_eq!(grid_facet_text_counts(&facets), vec![("ready".to_string(), 1)]);

        // The pipelineTime facet exposes the exact pinned millisecond duration.
        let time_facets = grid_facet_page_with_ranges(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::PipelineTime,
            &DocumentListSearchRanges::default(),
            50,
        )
        .await?;
        let numbers: Vec<(i64, i64)> = time_facets
            .rows
            .iter()
            .filter_map(|row| row.value_number.map(|value| (value, row.facet_count)))
            .collect();
        assert!(
            numbers.contains(&(90125, 1)),
            "pipeline facet must expose the pinned displayed duration: {numbers:?}"
        );

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-225: the cost facet fails closed on a mixed-currency document anywhere
/// in the authorized, filtered scope — not only when the NULL group happens to
/// land on the returned page.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn cost_facet_guard_scans_full_scope_not_just_the_returned_page() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentFacetColumn, DocumentListCountQuery, DocumentListFilters, DocumentListSearchRanges,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let finish = snapshot - chrono::Duration::minutes(1);
        let mut docs = Vec::new();
        for (index, total) in [(0_i64, 1_i64), (1, 5), (2, 9)] {
            let id = seed_grid_document(
                &pool,
                &fixture,
                &format!("guard-{index}"),
                &format!("guard-{index}.txt"),
                Some("text/plain"),
                10,
            )
            .await?;
            let job = seed_grid_attempt(&pool, &fixture, id, finish, Some(finish)).await?;
            seed_grid_cost(&pool, &fixture, job, id, "USD", total).await?;
            docs.push((id, job));
        }
        let filters = DocumentListFilters::default();
        let ranges = DocumentListSearchRanges::default();

        // All single-currency: the scope is clean.
        let clean = grid_scope_read(
            &pool,
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?
        .has_null_cost;
        assert!(!clean, "a single-currency scope must not report a null cost");

        // The first facet page would still look clean, yet one document is
        // multi-currency: the guard must catch it before pagination.
        let first_page = grid_facet_page_with_ranges(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Cost,
            &ranges,
            1,
        )
        .await?;
        assert_eq!(first_page.rows.len(), 1);
        assert!(
            first_page.rows[0].value_decimal.is_some(),
            "the first cost page advertises a clean value"
        );

        // A second currency on the same document, carried by a distinct
        // execution so the per-execution unique key holds.
        sqlx::query(
            "insert into billing_execution_cost
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              knowledge_document_id, total_cost, currency_code, provider_call_count,
              unpriced_provider_call_count)
             values ('ingest_attempt', $1, $2, $3, $4, 2, 'EUR', 1, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(docs[1].0)
        .execute(&pool)
        .await?;
        let mixed = grid_scope_read(
            &pool,
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?
        .has_null_cost;
        assert!(mixed, "a mixed-currency document anywhere in scope must be detected");
        // The library health alone does not catch a per-document mixed currency.
        let health = grid_scope_read(
            &pool,
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?;
        assert!(!health.rollup_dirty && health.terminal_error_code.is_none());

        // A dirty rollup is caught by the health read the guard also runs.
        sqlx::query(
            "insert into billing_execution_cost_rollup_state
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              dirty_generation, applied_generation, dirty_at, repair_attempts,
              next_attempt_at, last_error)
             values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .execute(&pool)
        .await?;
        let dirty_now = grid_scope_read(
            &pool,
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?
        .rollup_dirty;
        assert!(dirty_now, "a dirty rollup must be reported by the health read");

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-236: library rollup health must be observable even when the page's own
/// filter resolves to zero rows. The single statement that answers the page
/// also carries health, so a concurrent dirty generation cannot be hidden by an
/// empty page — the class the old first-row guard fell into.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_scope_health_is_observable_with_zero_rows() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentListCountQuery, DocumentListFilters, DocumentListSearchRanges,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let filters = DocumentListFilters::default();
        let ranges = DocumentListSearchRanges::default();

        // A scope that matches no documents: an id filter resolving to a
        // non-existent document.
        let empty_scope = || DocumentListCountQuery {
            library_id: fixture.library_id,
            include_deleted: false,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &filters,
            search_ranges: &ranges,
            status_filter: &[],
            id_filter: Some(&[]),
            snapshot_at: snapshot,
        };

        let clean = grid_scope_read(&pool, empty_scope()).await?;
        assert!(!clean.rollup_dirty, "a clean library is not dirty");
        assert_eq!(clean.row_count, 0, "the empty id filter matches no rows");

        // Dirty the library rollup; the empty scope must still report it.
        sqlx::query(
            "insert into billing_execution_cost_rollup_state
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              dirty_generation, applied_generation, dirty_at, repair_attempts,
              next_attempt_at, last_error)
             values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .execute(&pool)
        .await?;
        let dirty = grid_scope_read(&pool, empty_scope()).await?;
        assert!(
            dirty.rollup_dirty,
            "health must be observable through the same statement even with zero rows"
        );
        assert_eq!(dirty.row_count, 0);

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-236: a clean cost-filtered list read reports the filter's own total, not
/// the cost-excluded scope total. The consolidated read passes `exclude = None`
/// so the cost filter narrows the total exactly as it narrows the rows.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_scope_total_respects_cost_filter() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentListCountQuery, DocumentListFilters, DocumentListPageQuery,
        DocumentListSearchRanges, DocumentListSortColumn, FilterValues,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let finish = snapshot - chrono::Duration::minutes(1);
        for (index, total) in [(0_i64, 5_i64), (1, 5), (2, 5), (3, 9)] {
            let id = seed_grid_document(
                &pool,
                &fixture,
                &format!("cost-filter-{index}"),
                &format!("cost-filter-{index}.txt"),
                Some("text/plain"),
                10,
            )
            .await?;
            let job = seed_grid_attempt(&pool, &fixture, id, finish, Some(finish)).await?;
            seed_grid_cost(&pool, &fixture, job, id, "USD", total).await?;
        }
        let filters = DocumentListFilters {
            cost: FilterValues {
                present: true,
                include_null: false,
                values: vec![rust_decimal::Decimal::new(5, 0)],
            },
            ..Default::default()
        };
        let search_ranges = DocumentListSearchRanges::default();
        let read = content_document_list_repository::read_document_list_page(
            &pool,
            DocumentListPageQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                cursor: None,
                limit: 50,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &search_ranges,
                snapshot_at: snapshot,
                sort: DocumentListSortColumn::CreatedAt,
                sort_desc: true,
                status_filter: &[],
                id_filter: None,
            },
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &search_ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?;
        assert_eq!(
            read.scope.row_count, 3,
            "the scope total must honor the cost filter, not ignore it"
        );
        assert_eq!(read.page.rows.len(), 3, "the same filter narrows the rows");
        // The six derived-status buckets come from the same scope aggregate and
        // the same cost filter. Every seeded document is `ready`.
        let counts = read.scope.status_counts();
        assert_eq!(counts.total, Some(3), "the bucket total honors the cost filter");
        assert_eq!(counts.ready, Some(3));
        assert_eq!(counts.processing, Some(0));
        assert_eq!(counts.queued, Some(0));
        assert_eq!(counts.failed, Some(0));
        assert_eq!(counts.canceled, Some(0));
        assert_eq!(counts.empty, Some(0));
        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-236: a non-cost facet stays available while the rollup reconciles and
/// reports a truthful `reconciling` marker instead of a hardcoded `current`.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn non_cost_facet_is_available_and_reconciling_when_dirty() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentFacetColumn, DocumentListCountQuery, DocumentListFacetQuery, DocumentListFilters,
        DocumentListSearchRanges,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        for (index, name) in [(0, "facet-a.txt"), (1, "facet-b.txt")] {
            seed_grid_document(
                &pool,
                &fixture,
                &format!("facet-{index}"),
                name,
                Some("text/plain"),
                10,
            )
            .await?;
        }
        sqlx::query(
            "insert into billing_execution_cost_rollup_state
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              dirty_generation, applied_generation, dirty_at, repair_attempts,
              next_attempt_at, last_error)
             values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .execute(&pool)
        .await?;
        let filters = DocumentListFilters::default();
        let ranges = DocumentListSearchRanges::default();
        let read = content_document_list_repository::read_document_facet_page(
            &pool,
            DocumentListFacetQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                column: DocumentFacetColumn::FileName,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                facet_search: None,
                facet_search_statuses: &[],
                facet_search_range: None,
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                snapshot_at: snapshot,
                cursor: None,
                limit: 50,
                id_filter: None,
            },
            DocumentListCountQuery {
                library_id: fixture.library_id,
                include_deleted: false,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                status_filter: &[],
                id_filter: None,
                snapshot_at: snapshot,
            },
        )
        .await?;
        assert!(read.rollup_dirty, "health is reported for a non-cost facet too");
        assert_eq!(
            read.page.rows.len(),
            2,
            "a non-cost facet stays available while the rollup reconciles"
        );
        assert!(
            !content_document_list_repository::document_scope_reads_cost(
                None,
                None,
                None,
                &filters,
                Some(DocumentFacetColumn::FileName),
            ),
            "a non-cost facet with no cost predicate is not scope-pending"
        );
        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// IRG-236: the scope facts and the page rows come from one REPEATABLE READ
/// snapshot. A dirty generation committed between the two statements on a
/// second connection must not appear in the page the scope facts describe.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_page_scope_and_rows_share_one_snapshot_across_dirty_generation() -> anyhow::Result<()>
{
    use content_document_list_repository::{
        DocumentListCountQuery, DocumentListFilters, DocumentListPageQuery,
        DocumentListSearchRanges, DocumentListSortColumn,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let finish = snapshot - chrono::Duration::minutes(1);
        let id = seed_grid_document(
            &pool,
            &fixture,
            "snapshot-0",
            "snapshot-0.txt",
            Some("text/plain"),
            10,
        )
        .await?;
        let job = seed_grid_attempt(&pool, &fixture, id, finish, Some(finish)).await?;
        seed_grid_cost(&pool, &fixture, job, id, "USD", 5).await?;

        let filters = DocumentListFilters::default();
        let search_ranges = DocumentListSearchRanges::default();
        let page_query = DocumentListPageQuery {
            library_id: fixture.library_id,
            include_deleted: false,
            cursor: None,
            limit: 10,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &filters,
            search_ranges: &search_ranges,
            snapshot_at: snapshot,
            sort: DocumentListSortColumn::CreatedAt,
            sort_desc: true,
            status_filter: &[],
            id_filter: None,
        };
        let count_query = DocumentListCountQuery {
            library_id: fixture.library_id,
            include_deleted: false,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            filters: &filters,
            search_ranges: &search_ranges,
            status_filter: &[],
            id_filter: None,
            snapshot_at: snapshot,
        };

        let dirty_pool = pool.clone();
        let workspace_id = fixture.workspace_id;
        let library_id = fixture.library_id;
        let read = content_document_list_repository::read_document_list_page_after_scope_read(
            &pool,
            page_query.clone(),
            count_query.clone(),
            || async move {
                sqlx::query(
                    "insert into billing_execution_cost_rollup_state
                     (owning_execution_kind, owning_execution_id, workspace_id, library_id,
                      dirty_generation, applied_generation, dirty_at, repair_attempts,
                      next_attempt_at, last_error)
                     values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
                )
                .bind(Uuid::now_v7())
                .bind(workspace_id)
                .bind(library_id)
                .execute(&dirty_pool)
                .await
                .expect("dirty-generation insert on the second connection must succeed");
            },
        )
        .await?;
        assert!(
            !read.scope.rollup_dirty,
            "the scope facts come from the snapshot taken before the dirty commit"
        );
        assert_eq!(read.page.rows.len(), 1);
        assert!(
            !read.page.rows[0].billing_rollup_dirty,
            "the page rows share that snapshot, so the row still reports the old generation"
        );

        // A fresh read observes the committed dirty generation.
        let after = content_document_list_repository::read_document_list_page(
            &pool,
            page_query,
            count_query,
        )
        .await?;
        assert!(after.scope.rollup_dirty, "the dirty generation is visible to a new snapshot");
        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn grid_facet_page_hinted(
    pool: &PgPool,
    fixture: &ContentRepositoryFixture,
    snapshot_at: chrono::DateTime<Utc>,
    column: content_document_list_repository::DocumentFacetColumn,
    facet_search: Option<&str>,
    facet_search_statuses: &[String],
    facet_search_range: Option<(String, String)>,
    filters: &content_document_list_repository::DocumentListFilters,
    limit: u32,
) -> anyhow::Result<content_document_list_repository::DocumentFacetPage> {
    content_document_list_repository::list_document_facet_rows(
        pool,
        content_document_list_repository::DocumentListFacetQuery {
            library_id: fixture.library_id,
            include_deleted: false,
            column,
            search: None,
            normalized_search: None,
            search_statuses: &[],
            facet_search,
            facet_search_statuses,
            facet_search_range,
            filters,
            search_ranges: &content_document_list_repository::DocumentListSearchRanges::default(),
            status_filter: &[],
            snapshot_at,
            cursor: None,
            limit,
            id_filter: None,
        },
    )
    .await
    .map_err(Into::into)
}

/// IRG-225: facet-local typed hints match this facet's OWN displayed value,
/// OR with `facetSearch`, without touching the main scope.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_facet_typed_hints_match_own_displayed_values() -> anyhow::Result<()> {
    use content_document_list_repository::{
        DocumentFacetColumn, DocumentListFilters, FilterValues,
    };
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let finish = snapshot - chrono::Duration::minutes(5);
        let _a = seed_grid_dated_attempt(
            &pool,
            &fixture,
            GridAttemptSeed {
                key: "hint-a",
                file_name: "hint-a.pdf",
                mime_type: "application/pdf",
                byte_size: 1537,
                started_at: finish,
                duration_ms: 90125,
            },
        )
        .await?;
        let _b = seed_grid_dated_attempt(
            &pool,
            &fixture,
            GridAttemptSeed {
                key: "hint-b",
                file_name: "hint-b.txt",
                mime_type: "text/plain",
                byte_size: 64,
                started_at: finish - chrono::Duration::hours(1),
                duration_ms: 500,
            },
        )
        .await?;

        let empty =
            seed_grid_document(&pool, &fixture, "hint-empty", "hint-empty.bin", None, 0).await?;
        sqlx::query("update content_document set created_at = $2 where id = $1")
            .bind(empty)
            .bind(finish - chrono::Duration::hours(2))
            .execute(&pool)
            .await?;

        let no_filters = DocumentListFilters::default();

        // Size facet range matches the 1537-byte value only.
        let size = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::FileSize,
            None,
            &[],
            Some(("1485".to_string(), "1588".to_string())),
            &no_filters,
            50,
        )
        .await?;
        let size_numbers: Vec<(i64, i64)> = size
            .rows
            .iter()
            .filter_map(|row| row.value_number.map(|value| (value, row.facet_count)))
            .collect();
        assert_eq!(size_numbers, vec![(1537, 1)]);

        // Duration facet range matches the 90125 ms value only.
        let duration = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::PipelineTime,
            None,
            &[],
            Some(("90000".to_string(), "91000".to_string())),
            &no_filters,
            50,
        )
        .await?;
        let duration_numbers: Vec<(i64, i64)> = duration
            .rows
            .iter()
            .filter_map(|row| row.value_number.map(|value| (value, row.facet_count)))
            .collect();
        assert_eq!(duration_numbers, vec![(90125, 1)]);

        // A range that contains A's *finish* but not its *upload* selects
        // nothing on UploadedAt (own timestamp only) and A on FinishedAt.
        let finished_only = (
            (finish + chrono::Duration::seconds(1)).to_rfc3339(),
            (finish + chrono::Duration::seconds(200)).to_rfc3339(),
        );
        let uploaded = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::UploadedAt,
            None,
            &[],
            Some(finished_only.clone()),
            &no_filters,
            50,
        )
        .await?;
        assert!(
            uploaded.rows.is_empty(),
            "the uploaded_at facet must match upload timestamps, not finish timestamps"
        );
        let finished = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::FinishedAt,
            None,
            &[],
            Some(finished_only),
            &no_filters,
            50,
        )
        .await?;
        assert_eq!(
            finished.rows.iter().filter_map(|row| row.value_timestamp).count(),
            1,
            "the finished_at facet must match the finish timestamp"
        );

        // Status facet: typed status hint; raw facetSearch; and their OR.
        let ready = vec!["ready".to_string()];
        let typed_status = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Status,
            None,
            &ready,
            None,
            &no_filters,
            50,
        )
        .await?;
        assert_eq!(grid_facet_text_counts(&typed_status), vec![("ready".to_string(), 2)]);

        let raw_search = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Status,
            Some("empt"),
            &[],
            None,
            &no_filters,
            50,
        )
        .await?;
        assert_eq!(grid_facet_text_counts(&raw_search), vec![("empty".to_string(), 1)]);

        let empty_hint = vec!["empty".to_string()];
        let or_hints = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Status,
            Some("ready"),
            &empty_hint,
            None,
            &no_filters,
            50,
        )
        .await?;
        assert_eq!(
            grid_facet_text_counts(&or_hints),
            vec![("empty".to_string(), 1), ("ready".to_string(), 2)],
            "raw facetSearch and the typed status hint are OR alternatives"
        );

        // The main column filter still ANDs: a ready-only main filter keeps the
        // size facet to ready documents.
        let ready_only = DocumentListFilters {
            status: FilterValues {
                present: true,
                include_null: false,
                values: vec!["ready".to_string()],
            },
            ..Default::default()
        };
        let scoped = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::FileSize,
            None,
            &[],
            Some(("1485".to_string(), "1588".to_string())),
            &ready_only,
            50,
        )
        .await?;
        let scoped_numbers: Vec<i64> =
            scoped.rows.iter().filter_map(|row| row.value_number).collect();
        assert_eq!(scoped_numbers, vec![1537]);

        // Self-exclusion kept: the Status facet ignores its own column filter.
        let self_excluded = grid_facet_page_hinted(
            &pool,
            &fixture,
            snapshot,
            DocumentFacetColumn::Status,
            None,
            &[],
            None,
            &ready_only,
            50,
        )
        .await?;
        assert_eq!(
            grid_facet_text_counts(&self_excluded),
            vec![("empty".to_string(), 1), ("ready".to_string(), 2)]
        );

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

/// Walk every page of the `derived_status` keyset, one public cursor at a time,
/// asserting no document is skipped or repeated.
async fn collect_status_keyset_ids(
    pool: &PgPool,
    library_id: Uuid,
    descending: bool,
    limit: u32,
    snapshot_at: chrono::DateTime<Utc>,
) -> anyhow::Result<Vec<Uuid>> {
    use content_document_list_repository::{
        DocumentListFilters, DocumentListPageCursor, DocumentListSearchRanges,
    };
    let filters = DocumentListFilters::default();
    let ranges = DocumentListSearchRanges::default();
    let mut cursor = None;
    let mut ids = Vec::new();
    for _ in 0..64 {
        let page = content_document_list_repository::list_document_page_rows(
            pool,
            content_document_list_repository::DocumentListPageQuery {
                library_id,
                include_deleted: false,
                cursor: cursor.as_ref(),
                limit,
                search: None,
                normalized_search: None,
                search_statuses: &[],
                filters: &filters,
                search_ranges: &ranges,
                snapshot_at,
                sort: content_document_list_repository::DocumentListSortColumn::DerivedStatus,
                sort_desc: descending,
                status_filter: &[],
                id_filter: None,
            },
        )
        .await?;
        for row in &page.rows {
            assert!(!ids.contains(&row.id), "the status cursor repeated a document");
            ids.push(row.id);
        }
        if !page.has_more {
            return Ok(ids);
        }
        let last = page.rows.last().expect("a page with has_more has a last row");
        // The seeded library is uniformly `ready`, so every cursor carries the
        // same status text.
        cursor = Some(DocumentListPageCursor {
            created_at: last.created_at,
            document_id: last.id,
            text_value: Some("ready".to_string()),
            number_value: None,
            finished_value: None,
            value_is_null: false,
        });
    }
    anyhow::bail!("the status keyset did not finish within the page bound")
}

/// IRG-225: the `derived_status` ASC keyset must follow the `created_at desc`
/// tie-break of its ORDER BY (`status asc, created_at desc, id desc`), not a
/// plain tuple `>`. Equal-status documents paged by a small limit must not be
/// skipped or repeated.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_status_asc_keyset_does_not_skip_or_repeat_equal_status_rows() -> anyhow::Result<()>
{
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = ContentRepositoryFixture::create(&pool).await?;
    let result = async {
        let snapshot = Utc::now();
        let mut seeded = Vec::new();
        for index in 0..7_i64 {
            let id = seed_grid_document(
                &pool,
                &fixture,
                &format!("status-{index}"),
                &format!("status-{index}.txt"),
                Some("text/plain"),
                10,
            )
            .await?;
            // Three created_at clusters, so equal-status rows tie on
            // created_at and only the id tie-break distinguishes them.
            let created = snapshot - chrono::Duration::seconds(index % 3);
            sqlx::query("update content_document set created_at = $2 where id = $1")
                .bind(id)
                .bind(created)
                .execute(&pool)
                .await?;
            seeded.push((id, created));
        }
        // Expected order for equal status: created_at desc, id desc (the ORDER
        // BY tie-break, identical for asc and desc status directions).
        let mut expected = seeded.clone();
        expected.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
        let expected_ids: Vec<Uuid> = expected.iter().map(|(id, _)| *id).collect();

        let ascending =
            collect_status_keyset_ids(&pool, fixture.library_id, false, 2, snapshot).await?;
        assert_eq!(ascending, expected_ids, "ASC status pages must cover every row exactly once");
        let ascending_unique: std::collections::HashSet<Uuid> = ascending.iter().copied().collect();
        assert_eq!(ascending_unique.len(), ascending.len(), "no duplicate documents across pages");

        let descending =
            collect_status_keyset_ids(&pool, fixture.library_id, true, 2, snapshot).await?;
        assert_eq!(descending, expected_ids, "DESC status pages must cover every row exactly once");

        anyhow::Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}
