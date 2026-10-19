use crate::{content_lifecycle_support::ContentLifecycleFixture, temp_postgres_database};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::config::Settings,
    infra::{
        knowledge_plane::{
            DocumentPointersUpdate, DocumentStore, IngestAttemptMutationOutcome,
            IngestRevisionExtractArtifact, IngestRevisionWriteAuthority,
        },
        knowledge_rows::{
            KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
        },
        postgres::pg_document_store::PgDocumentStore,
        repositories::{
            catalog_repository, content_chunk_repository, content_document_repository,
            content_knowledge_projection_repository, content_mutation_repository,
            content_revision_repository, ingest_repository,
        },
    },
    services::content::service::{
        AppendInlineMutationCommand, CreateDocumentCommand, CreateRevisionCommand,
        PromoteHeadCommand,
    },
};

fn authority(
    workspace_id: Uuid,
    library_id: Uuid,
    attempt_id: Uuid,
    revision_id: Uuid,
) -> IngestRevisionWriteAuthority {
    IngestRevisionWriteAuthority { workspace_id, library_id, attempt_id, revision_id }
}

fn structured_artifact(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    marker: &str,
) -> (KnowledgeStructuredRevisionRow, KnowledgeStructuredBlockRow) {
    let now = Utc::now();
    let block_id = Uuid::new_v5(&revision_id, marker.as_bytes());
    (
        KnowledgeStructuredRevisionRow {
            revision_id,
            workspace_id,
            library_id,
            document_id,
            preparation_state: marker.to_string(),
            normalization_profile: format!("neutral-profile-{marker}"),
            source_format: "text".to_string(),
            language_code: None,
            block_count: 1,
            chunk_count: 1,
            typed_fact_count: 1,
            outline_json: serde_json::json!([{ "blockId": block_id, "marker": marker }]),
            prepared_at: now,
            updated_at: now,
        },
        KnowledgeStructuredBlockRow {
            block_id,
            workspace_id,
            library_id,
            document_id,
            revision_id,
            ordinal: 0,
            block_kind: "paragraph".to_string(),
            text: format!("{marker} structured evidence"),
            normalized_text: format!("{marker} structured evidence"),
            heading_trail: vec![marker.to_string()],
            section_path: vec![marker.to_string()],
            page_number: None,
            span_start: Some(0),
            span_end: Some(24),
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: now,
            updated_at: now,
        },
    )
}

fn chunk_projection(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    support_block_id: Uuid,
    marker: &str,
) -> content_chunk_repository::NewContentChunkProjection {
    content_chunk_repository::NewContentChunkProjection {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        chunk_index: 0,
        start_offset: 0,
        end_offset: 20,
        token_count: Some(3),
        chunk_kind: Some("paragraph".to_string()),
        content_text: format!("{marker} chunk evidence"),
        normalized_text: format!("{marker} chunk evidence"),
        text_checksum: format!("sha256:{marker}"),
        support_block_ids: vec![support_block_id],
        section_path: vec![marker.to_string()],
        heading_trail: vec![marker.to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: None,
        quality_score: Some(1.0),
        window_text: Some(format!("{marker} chunk evidence")),
        occurred_at: None,
        occurred_until: None,
    }
}

/// The row identity and supporting evidence for one seeded technical fact.
struct TechnicalFactSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    support_block_id: Uuid,
    support_chunk_id: Uuid,
    marker: &'a str,
}

fn technical_fact(seed: TechnicalFactSeed<'_>) -> KnowledgeTechnicalFactRow {
    let now = Utc::now();
    KnowledgeTechnicalFactRow {
        fact_id: Uuid::new_v5(&seed.revision_id, format!("fact:{}", seed.marker).as_bytes()),
        workspace_id: seed.workspace_id,
        library_id: seed.library_id,
        document_id: seed.document_id,
        revision_id: seed.revision_id,
        fact_kind: "identifier".to_string(),
        normalized_value_text: seed.marker.to_string(),
        normalized_value_exact: seed.marker.to_string(),
        normalized_value_json: serde_json::json!({ "value": seed.marker }),
        display_value: seed.marker.to_string(),
        qualifiers_json: serde_json::json!([]),
        support_block_ids: vec![seed.support_block_id],
        support_chunk_ids: vec![seed.support_chunk_id],
        confidence: Some(1.0),
        extraction_kind: "deterministic".to_string(),
        conflict_group_id: None,
        created_at: now,
        updated_at: now,
    }
}

struct ContentMutationRevisionScope {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
}

/// Creates the upload mutation, its pending item and the leased
/// content-mutation job the fencing tests drive directly, without a worker.
async fn create_leased_content_mutation_job(
    pool: &PgPool,
    scope: ContentMutationRevisionScope,
    dedupe_key: String,
    file_name: &str,
) -> Result<ingest_repository::IngestJobRow> {
    let ContentMutationRevisionScope { workspace_id, library_id, document_id, revision_id } = scope;
    let revision = content_revision_repository::get_revision_by_id(pool, revision_id)
        .await?
        .context("preparation fixture revision missing")?;
    anyhow::ensure!(
        (revision.workspace_id, revision.library_id, revision.document_id)
            == (workspace_id, library_id, document_id),
        "preparation fixture revision scope mismatch"
    );
    let request_identity = serde_json::json!({
        "workspaceId":workspace_id,"libraryId":library_id,"operationKind":"upload","sourceIdentity":null,
        "target":{"kind":"existing","documentId":document_id},
        "revision":{"contentSourceKind":revision.content_source_kind,"baseRevisionId":null,"visibilityIntentId":null},
        "replacement":{"checksum":revision.checksum,"byteSize":revision.byte_size,"mimeType":revision.mime_type,"fileName":file_name,
            "title":revision.title,"languageCode":revision.language_code,"sourceUri":revision.source_uri,"documentHint":revision.document_hint},
    });
    let mutation = content_mutation_repository::create_mutation(
        pool,
        &content_mutation_repository::NewContentMutation {
            request_identity: &request_identity,
            workspace_id,
            library_id,
            operation_kind: "upload",
            requested_by_principal_id: None,
            request_surface: "internal",
            idempotency_key: None,
            source_identity: None,
            mutation_state: "running",
            failure_code: None,
            conflict_code: None,
        },
    )
    .await?;
    let mutation_item = content_mutation_repository::create_mutation_item(
        pool,
        &content_mutation_repository::NewContentMutationItem {
            mutation_id: mutation.id,
            document_id,
            base_revision_id: None,
            result_revision_id: Some(revision_id),
            item_state: "pending",
            message: None,
        },
    )
    .await?;
    let job = ingest_repository::create_ingest_job(
        pool,
        &ingest_repository::NewIngestJob {
            workspace_id,
            library_id,
            mutation_id: Some(mutation.id),
            mutation_item_id: Some(mutation_item.id),
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(document_id),
            knowledge_revision_id: Some(revision_id),
            job_kind: "content_mutation".to_string(),
            queue_state: "leased".to_string(),
            priority: 10,
            dedupe_key: Some(dedupe_key),
            queued_at: Some(Utc::now()),
            available_at: Some(Utc::now()),
            completed_at: None,
        },
    )
    .await?;
    Ok(job)
}

/// Creates one leased attempt for a job from
/// [`create_leased_content_mutation_job`].
async fn lease_content_mutation_attempt(
    pool: &PgPool,
    job_id: Uuid,
    attempt_number: i32,
    lease_token: &str,
    progress_percent: i32,
) -> Result<ingest_repository::IngestAttemptRow> {
    let attempt = ingest_repository::create_ingest_attempt(
        pool,
        &ingest_repository::NewIngestAttempt {
            job_id,
            attempt_number,
            worker_principal_id: None,
            lease_token: Some(lease_token.to_string()),
            knowledge_generation_id: None,
            attempt_state: "leased".to_string(),
            current_stage: Some("prepare_structure".to_string()),
            started_at: Some(Utc::now()),
            heartbeat_at: Some(Utc::now()),
            finished_at: None,
            failure_class: None,
            failure_code: None,
            failure_message: None,
            progress_percent,
            retryable: true,
        },
    )
    .await?;
    Ok(attempt)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn stale_attempt_cannot_replace_any_persisted_preparation_artifact() -> Result<()> {
    let settings = Settings::from_env().context("load ingest preparation fencing settings")?;
    let database =
        TempPostgresDatabase::create(&settings.database_url, "ingest_preparation_fencing").await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let result = Box::pin(async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("preparation-fence-workspace-{suffix}"),
            "Preparation Fence Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("preparation-fence-library-{suffix}"),
            "Preparation Fence Library",
            None,
            None,
        )
        .await?;
        let document = content_knowledge_projection_repository::create_document_with_projection(
            &pool,
            &content_document_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("preparation-fence-document-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("preparation-fence.txt"),
        )
        .await?;
        let revision = match content_revision_repository::create_revision_with_projection(
            &pool,
            &content_revision_repository::NewContentRevisionProjection {
                admission_identity: Some(&crate::content_repository::unbound_mutation_admission(
                    ironrag_contracts::documents::ReplacementIdentity {
                        checksum: format!("sha256:{}", "c".repeat(64)), mime_type: "text/plain".to_owned(),
                        byte_size: 64, file_name: "preparation-fence.txt".to_owned(),
                        title: Some("Preparation Fence Fixture".to_owned()),
                        language_code: None, source_uri: None, document_hint: None,
                    },
                )?),
                document_id: document.id,
                workspace_id: workspace.id,
                library_id: library.id,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "c".repeat(64)),
                mime_type: "text/plain",
                byte_size: 64,
                title: Some("Preparation Fence Fixture"),
                language_code: None,
                source_uri: None,
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await?
        {
            content_revision_repository::CreateContentRevisionOutcome::Created(revision) => {
                *revision
            }
            outcome => return Err(anyhow!("revision fixture was not created: {outcome:?}")),
        };
        let job = create_leased_content_mutation_job(
            &pool,
            ContentMutationRevisionScope {
                workspace_id: workspace.id,
                library_id: library.id,
                document_id: document.id,
                revision_id: revision.id,
            },
            format!("preparation-fence:{suffix}"),
            "preparation-fence.txt",
        )
        .await?;
        let old_attempt =
            lease_content_mutation_attempt(&pool, job.id, 1, "preparation-old", 30).await?;
        let new_attempt =
            lease_content_mutation_attempt(&pool, job.id, 2, "preparation-new", 40).await?;
        let old = authority(workspace.id, library.id, old_attempt.id, revision.id);
        let current = authority(workspace.id, library.id, new_attempt.id, revision.id);
        let store = PgDocumentStore { pool: pool.clone() };

        let current_extract = store
            .update_revision_extract_artifact_fenced(
                current,
                "text_readable",
                IngestRevisionExtractArtifact::Replace {
                    normalized_text: "current normalized text",
                    text_checksum: "sha256:current-text",
                    text_readable_at: Some(Utc::now()),
                    image_checksum: Some("sha256:current-image"),
                },
            )
            .await?;
        assert!(matches!(current_extract, IngestAttemptMutationOutcome::Applied(_)));
        let revision_with_image = store.get_revision(revision.id).await?.context("revision")?;
        assert_eq!(revision_with_image.image_checksum.as_deref(), Some("sha256:current-image"));
        let current_extract_without_image = store
            .update_revision_extract_artifact_fenced(
                current,
                "text_readable",
                IngestRevisionExtractArtifact::Replace {
                    normalized_text: "current normalized text",
                    text_checksum: "sha256:current-text",
                    text_readable_at: Some(Utc::now()),
                    image_checksum: None,
                },
            )
            .await?;
        assert!(matches!(current_extract_without_image, IngestAttemptMutationOutcome::Applied(_)));
        store
            .update_document_pointers(DocumentPointersUpdate {
                document_id: document.id,
                document_state: "active",
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                latest_revision_no: Some(i64::from(revision.revision_number)),
                title: Some("current projection"),
                deleted_at: None,
            })
            .await?
            .context("current document projection")?;
        sqlx::query("update knowledge_document set file_name = null where document_id = $1")
            .bind(document.id)
            .execute(&pool)
            .await?;
        let mut stale_document_projection =
            store.get_document(document.id).await?.context("document projection")?;
        assert_eq!(stale_document_projection.file_name, None);
        let projection_timestamp_before_repair = stale_document_projection.updated_at;
        stale_document_projection.updated_at = stale_document_projection.created_at;
        stale_document_projection.file_name = Some("untrusted-stale-name.txt".to_string());
        stale_document_projection.document_state = "processing".to_string();
        stale_document_projection.active_revision_id = None;
        stale_document_projection.readable_revision_id = None;
        stale_document_projection.latest_revision_no = None;
        let mut stale_revision_projection =
            store.get_revision(revision.id).await?.context("revision projection")?;
        stale_revision_projection.normalized_text = None;
        stale_revision_projection.text_checksum = None;
        stale_revision_projection.text_state = "accepted".to_string();
        let stale_self_heal = store
            .ensure_ingest_revision_projection_fenced(
                old,
                &stale_document_projection,
                &stale_revision_projection,
            )
            .await?;
        assert!(matches!(stale_self_heal, IngestAttemptMutationOutcome::AuthorityLost));
        let preserved_document =
            store.get_document(document.id).await?.context("preserved document projection")?;
        assert_eq!(
            preserved_document.file_name, None,
            "a stale attempt must not repair even a missing projection display name",
        );
        assert_eq!(preserved_document.active_revision_id, Some(revision.id));
        assert_eq!(preserved_document.readable_revision_id, Some(revision.id));
        let preserved_revision =
            store.get_revision(revision.id).await?.context("preserved revision projection")?;
        assert_eq!(preserved_revision.normalized_text.as_deref(), Some("current normalized text"));
        assert_eq!(preserved_revision.text_checksum.as_deref(), Some("sha256:current-text"));
        let stale_extract = store
            .update_revision_extract_artifact_fenced(
                old,
                "failed",
                IngestRevisionExtractArtifact::StateOnly,
            )
            .await?;
        assert!(matches!(stale_extract, IngestAttemptMutationOutcome::AuthorityLost));
        let persisted_revision = store.get_revision(revision.id).await?.context("revision")?;
        assert_eq!(persisted_revision.normalized_text.as_deref(), Some("current normalized text"));
        assert_eq!(persisted_revision.text_checksum.as_deref(), Some("sha256:current-text"));
        assert_eq!(persisted_revision.image_checksum, None);
        assert_eq!(persisted_revision.text_state, "text_readable");

        let current_self_heal = store
            .ensure_ingest_revision_projection_fenced(
                current,
                &stale_document_projection,
                &stale_revision_projection,
            )
            .await?;
        assert!(matches!(current_self_heal, IngestAttemptMutationOutcome::Applied(_)));
        let repaired_document =
            store.get_document(document.id).await?.context("repaired document projection")?;
        assert_eq!(
            repaired_document.file_name.as_deref(),
            Some("preparation-fence.txt"),
            "the exact-scoped authoritative source name must win over the caller payload",
        );
        assert!(
            repaired_document.updated_at >= projection_timestamp_before_repair,
            "display-name repair must never regress projection freshness to a stale caller timestamp",
        );
        assert_eq!(
            repaired_document.active_revision_id,
            Some(revision.id),
            "display-name repair must not regress the existing head",
        );

        sqlx::query(
            "update knowledge_document
             set file_name = 'preserved-canonical-name.txt'
             where document_id = $1",
        )
        .bind(document.id)
        .execute(&pool)
        .await?;
        stale_document_projection.file_name = Some("replacement-name.txt".to_string());
        let repeated_self_heal = store
            .ensure_ingest_revision_projection_fenced(
                current,
                &stale_document_projection,
                &stale_revision_projection,
            )
            .await?;
        assert!(matches!(repeated_self_heal, IngestAttemptMutationOutcome::Applied(_)));
        let preserved_named_document = store
            .get_document(document.id)
            .await?
            .context("named document projection")?;
        assert_eq!(
            preserved_named_document.file_name.as_deref(),
            Some("preserved-canonical-name.txt"),
            "self-heal must never overwrite a non-null projection display name",
        );

        let foreign_library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("preparation-fence-foreign-library-{suffix}"),
            "Preparation Fence Foreign Library",
            None,
            None,
        )
        .await?;
        sqlx::query(
            "update knowledge_document
             set workspace_id = $2, library_id = $3, file_name = null
             where document_id = $1",
        )
        .bind(document.id)
        .bind(workspace.id)
        .bind(foreign_library.id)
        .execute(&pool)
        .await?;
        let incompatible_scope_error = store
            .ensure_ingest_revision_projection_fenced(
                current,
                &stale_document_projection,
                &stale_revision_projection,
            )
            .await
            .expect_err("an incompatible existing projection scope must be rejected");
        assert!(
            incompatible_scope_error
                .to_string()
                .contains("incompatible authority scope")
        );
        let foreign_scoped_name = sqlx::query_scalar::<_, Option<String>>(
            "select file_name
             from knowledge_document
             where document_id = $1
               and workspace_id = $2
               and library_id = $3",
        )
        .bind(document.id)
        .bind(workspace.id)
        .bind(foreign_library.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            foreign_scoped_name, None,
            "a mismatched scope must not be repaired before rejection",
        );
        assert_eq!(
            store
                .repair_missing_document_file_names(workspace.id, library.id, 1)
                .await?,
            0,
            "bounded reconciliation must not cross its exact library scope",
        );
        sqlx::query(
            "update knowledge_document
             set workspace_id = $2, library_id = $3,
                 file_name = 'preserved-canonical-name.txt'
             where document_id = $1",
        )
        .bind(document.id)
        .bind(workspace.id)
        .bind(library.id)
        .execute(&pool)
        .await?;
        let foreign_delete = catalog_repository::delete_library(&pool, foreign_library.id).await?;
        assert!(matches!(
            foreign_delete,
            ironrag_backend::infra::repositories::catalog_repository::CatalogLibraryDeleteOutcome::Deleted
        ));
        let additional_document =
            content_knowledge_projection_repository::create_document_with_projection(
                &pool,
                &content_document_repository::NewContentDocument {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    external_key: &format!("preparation-fence-additional-{suffix}"),
                    document_state: "active",
                    created_by_principal_id: None,
                    parent_external_key: None,
                    parent_document_id: None,
                    document_role: "primary",
                },
                Some("preparation-fence-additional.txt"),
            )
            .await?;
        sqlx::query(
            "update knowledge_document
             set file_name = null
             where document_id = any($1)",
        )
            .bind([document.id, additional_document.id])
            .execute(&pool)
            .await?;
        assert_eq!(
            store
                .repair_missing_document_file_names(workspace.id, library.id, 1)
                .await?,
            1,
            "bounded reconciliation must repair only its requested batch",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from knowledge_document
                 where document_id = any($1)
                   and file_name is null",
            )
            .bind([document.id, additional_document.id])
            .fetch_one(&pool)
            .await?,
            1,
            "one candidate must remain after a one-row repair batch",
        );
        assert_eq!(
            store
                .repair_missing_document_file_names(workspace.id, library.id, 1)
                .await?,
            1,
            "the next bounded pass must repair the remaining exact-scoped row",
        );
        let repaired_file_names = sqlx::query_scalar::<_, Option<String>>(
            "select file_name
             from knowledge_document
             where document_id = any($1)
             order by file_name",
        )
        .bind([document.id, additional_document.id])
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            repaired_file_names,
            vec![
                Some("preparation-fence-additional.txt".to_string()),
                Some("preparation-fence.txt".to_string()),
            ],
        );
        assert_eq!(
            store
                .repair_missing_document_file_names(workspace.id, library.id, 1)
                .await?,
            0,
            "bounded reconciliation must be idempotent and preserve non-null names",
        );

        sqlx::query(
            "update knowledge_document
             set file_name = null
             where document_id = any($1)",
        )
        .bind([document.id, additional_document.id])
        .execute(&pool)
        .await?;
        let locked_document_id = [document.id, additional_document.id]
            .into_iter()
            .min()
            .context("concurrent repair fixture must have a first candidate")?;
        let unlocked_document_id = [document.id, additional_document.id]
            .into_iter()
            .max()
            .context("concurrent repair fixture must have a second candidate")?;
        let mut competing_transaction = pool.begin().await?;
        sqlx::query(
            "select document_id
             from knowledge_document
             where document_id = $1
             for update",
        )
        .bind(locked_document_id)
        .execute(&mut *competing_transaction)
        .await?;
        let concurrent_repaired = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.repair_missing_document_file_names(workspace.id, library.id, 1),
        )
        .await
        .context("SKIP LOCKED repair blocked behind a competing document lock")??;
        assert_eq!(concurrent_repaired, 1);
        let concurrent_names = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select document_id, file_name
             from knowledge_document
             where document_id = any($1)
             order by document_id",
        )
        .bind([document.id, additional_document.id])
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            concurrent_names
                .iter()
                .find(|(document_id, _)| *document_id == locked_document_id)
                .and_then(|(_, file_name)| file_name.as_deref()),
            None,
            "the contended first candidate must remain untouched",
        );
        assert!(
            concurrent_names
                .iter()
                .find(|(document_id, _)| *document_id == unlocked_document_id)
                .and_then(|(_, file_name)| file_name.as_deref())
                .is_some(),
            "the same bounded pass must promptly repair the next unlocked candidate",
        );
        competing_transaction.rollback().await?;

        let (mut current_metadata, mut current_block) =
            structured_artifact(workspace.id, library.id, document.id, revision.id, "current");
        current_metadata.preparation_state = "preparing".to_string();
        current_block.block_kind = "source_unit".to_string();
        for negative_metadata in [
            {
                let mut metadata = current_metadata.clone();
                metadata.block_count = -1;
                metadata
            },
            {
                let mut metadata = current_metadata.clone();
                metadata.chunk_count = -1;
                metadata
            },
            {
                let mut metadata = current_metadata.clone();
                metadata.typed_fact_count = -1;
                metadata
            },
        ] {
            assert!(
                store
                    .replace_structured_revision_artifact_fenced(
                        current,
                        &negative_metadata,
                        std::slice::from_ref(&current_block),
                    )
                    .await
                    .is_err(),
                "negative structured artifact counts must be rejected before publication"
            );
        }
        let (mut directly_prepared_metadata, directly_prepared_block) = structured_artifact(
            workspace.id,
            library.id,
            document.id,
            revision.id,
            "direct-prepared",
        );
        directly_prepared_metadata.preparation_state = "prepared".to_string();
        assert!(
            store
                .replace_structured_revision_artifact_fenced(
                    current,
                    &directly_prepared_metadata,
                    std::slice::from_ref(&directly_prepared_block),
                )
                .await
                .is_err(),
            "ingest replacement must not bypass finalization by writing prepared directly"
        );
        let current_structured = store
            .replace_structured_revision_artifact_fenced(
                current,
                &current_metadata,
                std::slice::from_ref(&current_block),
            )
            .await?;
        assert!(matches!(current_structured, IngestAttemptMutationOutcome::Applied(_)));
        let (mut stale_metadata, stale_block) =
            structured_artifact(workspace.id, library.id, document.id, revision.id, "stale");
        stale_metadata.preparation_state = "preparing".to_string();
        let stale_structured = store
            .replace_structured_revision_artifact_fenced(
                old,
                &stale_metadata,
                std::slice::from_ref(&stale_block),
            )
            .await?;
        assert!(matches!(stale_structured, IngestAttemptMutationOutcome::AuthorityLost));
        assert!(
            store.get_structured_revision(revision.id).await?.is_none(),
            "generic readers must not observe a preparing structured artifact"
        );
        assert!(
            store.list_structured_blocks_by_revision(revision.id).await?.is_empty(),
            "generic readers must not observe preparing blocks"
        );
        let persisted_metadata = sqlx::query_as::<_, (String, i32, i32)>(
            "select preparation_state, block_count::int4, chunk_count::int4
             from knowledge_structured_revision
             where revision_id = $1",
        )
        .bind(revision.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(persisted_metadata, ("preparing".to_string(), 1, 1));
        let persisted_block = sqlx::query_as::<_, (Uuid, String)>(
            "select block_id, text
             from knowledge_structured_block
             where revision_id = $1",
        )
        .bind(revision.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(persisted_block.0, current_block.block_id);
        assert_eq!(persisted_block.1, "current structured evidence");

        let mut current_chunk = chunk_projection(
            workspace.id,
            library.id,
            document.id,
            revision.id,
            current_block.block_id,
            "current",
        );
        let current_chunk_text =
            "current_code=204 endpoint=https://service.invalid/export?filter=literalneedle port=443 current chunk evidence";
        current_chunk.content_text = current_chunk_text.to_string();
        current_chunk.normalized_text = current_chunk_text.to_string();
        current_chunk.window_text = Some(current_chunk_text.to_string());
        let current_chunks = content_chunk_repository::replace_chunks_with_projection_fenced(
            &pool,
            current,
            std::slice::from_ref(&current_chunk),
        )
        .await?;
        let current_chunk_id = match current_chunks {
            IngestAttemptMutationOutcome::Applied(rows) => {
                assert_eq!(rows.len(), 1);
                rows[0].id
            }
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Err(anyhow!("current attempt unexpectedly lost chunk authority"));
            }
        };
        let stale_chunk = chunk_projection(
            workspace.id,
            library.id,
            document.id,
            revision.id,
            stale_block.block_id,
            "stale",
        );
        let stale_chunks = content_chunk_repository::replace_chunks_with_projection_fenced(
            &pool,
            old,
            std::slice::from_ref(&stale_chunk),
        )
        .await?;
        assert!(matches!(stale_chunks, IngestAttemptMutationOutcome::AuthorityLost));
        let content_chunks =
            content_chunk_repository::list_chunks_by_revision(&pool, revision.id).await?;
        assert_eq!(content_chunks.len(), 1);
        assert_eq!(content_chunks[0].id, current_chunk_id);
        assert_eq!(content_chunks[0].normalized_text, current_chunk_text);
        let knowledge_chunk = sqlx::query_as::<_, (Uuid, String, Vec<Uuid>)>(
            "select chunk_id, content_text, support_block_ids
             from knowledge_chunk
             where revision_id = $1",
        )
        .bind(revision.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(knowledge_chunk.0, current_chunk_id);
        assert_eq!(knowledge_chunk.1, current_chunk_text);
        assert_eq!(knowledge_chunk.2, vec![current_block.block_id]);
        let current_terms = vec!["current".to_string()];
        let code_terms = vec!["current_code".to_string()];
        let transport_terms = vec!["endpoint".to_string()];
        assert!(store.list_chunks_by_revision(revision.id).await?.is_empty());
        assert!(store.list_head_chunks_by_revision(revision.id, 1).await?.is_empty());
        assert_eq!(store.count_chunks_by_revision(revision.id).await?, 0);
        assert!(
            store
                .list_chunks_by_revision_matching_terms(revision.id, &current_terms, 1)
                .await?
                .is_empty()
        );
        assert!(
            store
                .list_chunks_by_revisions_matching_terms(&[revision.id], &current_terms, 1)
                .await?
                .is_empty()
        );
        assert!(store.list_chunks_by_revision_range(revision.id, 0, 0).await?.is_empty());
        assert!(store.list_chunks_by_revision_windows(revision.id, &[(0, 0)]).await?.is_empty());
        assert!(store.list_chunks_by_revisions_windows(&[(revision.id, 0, 0)]).await?.is_empty());
        assert!(store.list_tail_chunks_by_revision(revision.id, 1).await?.is_empty());
        assert!(store.get_chunk(current_chunk_id).await?.is_none());
        assert!(store.list_chunks_by_ids(&[current_chunk_id]).await?.is_empty());
        assert!(store.list_chunk_support_references_by_revision(revision.id).await?.is_empty());
        assert!(
            store
                .list_head_source_unit_blocks_by_revision(revision.id, 1, None, None, false)
                .await?
                .is_empty()
        );
        assert!(
            store
                .list_tail_source_unit_blocks_by_revision(revision.id, 1, None, None, false)
                .await?
                .is_empty()
        );
        assert!(
            store
                .search_code_pattern_chunks_by_terms(library.id, &[document.id], &code_terms, 1,)
                .await?
                .is_empty()
        );
        assert!(
            store
                .search_transport_pattern_chunks_by_terms(
                    library.id,
                    &[document.id],
                    &transport_terms,
                    1,
                )
                .await?
                .is_empty()
        );

        let current_fact = technical_fact(TechnicalFactSeed {
            workspace_id: workspace.id,
            library_id: library.id,
            document_id: document.id,
            revision_id: revision.id,
            support_block_id: current_block.block_id,
            support_chunk_id: current_chunk_id,
            marker: "current",
        });
        let current_facts = store
            .replace_technical_facts_fenced(current, std::slice::from_ref(&current_fact))
            .await?;
        assert!(matches!(current_facts, IngestAttemptMutationOutcome::Applied(_)));
        let invalid_replacement = [
            technical_fact(TechnicalFactSeed {
                workspace_id: workspace.id,
                library_id: library.id,
                document_id: document.id,
                revision_id: revision.id,
                support_block_id: current_block.block_id,
                support_chunk_id: current_chunk_id,
                marker: "rollback",
            }),
            technical_fact(TechnicalFactSeed {
                workspace_id: workspace.id,
                library_id: library.id,
                document_id: document.id,
                revision_id: revision.id,
                support_block_id: current_block.block_id,
                support_chunk_id: current_chunk_id,
                marker: "rollback",
            }),
        ];
        assert!(
            store.replace_technical_facts_fenced(current, &invalid_replacement).await.is_err(),
            "a duplicate fact id must reject the whole replacement"
        );
        let fact_id_after_rejected_replacement = sqlx::query_scalar::<_, Uuid>(
            "select fact_id from knowledge_technical_fact where revision_id = $1",
        )
        .bind(revision.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(fact_id_after_rejected_replacement, current_fact.fact_id);
        let stale_fact = technical_fact(TechnicalFactSeed {
            workspace_id: workspace.id,
            library_id: library.id,
            document_id: document.id,
            revision_id: revision.id,
            support_block_id: stale_block.block_id,
            support_chunk_id: Uuid::now_v7(),
            marker: "stale",
        });
        let stale_facts =
            store.replace_technical_facts_fenced(old, std::slice::from_ref(&stale_fact)).await?;
        assert!(matches!(stale_facts, IngestAttemptMutationOutcome::AuthorityLost));
        assert!(
            store.list_technical_facts_by_revision(revision.id).await?.is_empty(),
            "generic readers must not observe preparing facts"
        );
        let persisted_fact = sqlx::query_as::<_, (Uuid, String, Vec<Uuid>, Vec<Uuid>)>(
            "select fact_id, display_value, support_block_ids, support_chunk_ids
             from knowledge_technical_fact
             where revision_id = $1",
        )
        .bind(revision.id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(persisted_fact.0, current_fact.fact_id);
        assert_eq!(persisted_fact.1, "current");
        assert_eq!(persisted_fact.2, vec![current_block.block_id]);
        assert_eq!(persisted_fact.3, vec![current_chunk_id]);

        let stale_finalize =
            store.finalize_structured_revision_artifact_fenced(old, "sha256:current-text").await?;
        assert!(matches!(stale_finalize, IngestAttemptMutationOutcome::AuthorityLost));
        assert!(
            store
                .finalize_structured_revision_artifact_fenced(current, "sha256:wrong-text")
                .await
                .is_err(),
            "the finalizer must reject a checksum that does not match the extracted artifact"
        );
        for (column, mismatched_count) in
            [("block_count", 2_i32), ("chunk_count", 2_i32), ("typed_fact_count", 2_i32)]
        {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "update knowledge_structured_revision set {column} = $2 where revision_id = $1"
            )))
            .bind(revision.id)
            .bind(mismatched_count)
            .execute(&pool)
            .await?;
            assert!(
                store
                    .finalize_structured_revision_artifact_fenced(current, "sha256:current-text",)
                    .await
                    .is_err(),
                "the finalizer must reject a mismatched {column}"
            );
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "update knowledge_structured_revision set {column} = $2 where revision_id = $1"
            )))
            .bind(revision.id)
            .bind(1_i32)
            .execute(&pool)
            .await?;
        }
        let current_finalize = store
            .finalize_structured_revision_artifact_fenced(current, "sha256:current-text")
            .await?;
        assert!(matches!(current_finalize, IngestAttemptMutationOutcome::Applied(_)));
        let finalized_metadata = store
            .get_structured_revision(revision.id)
            .await?
            .context("finalized structured metadata")?;
        assert_eq!(finalized_metadata.preparation_state, "prepared");
        assert_eq!(store.list_structured_blocks_by_revision(revision.id).await?.len(), 1);
        assert_eq!(store.list_chunks_by_revision(revision.id).await?.len(), 1);
        assert_eq!(store.list_technical_facts_by_revision(revision.id).await?.len(), 1);
        // Keep one physical session so the restored policy and execution counters
        // describe the public calls, not another pool connection.
        let matching_pool = PgPoolOptions::new().max_connections(1)
            .connect(&database.database_url).await?;
        sqlx::query("set plan_cache_mode = force_generic_plan")
            .execute(&matching_pool).await?;
        let matching_store = PgDocumentStore { pool: matching_pool.clone() };
        for (terms, limit, expected) in [
            (current_terms.clone(), 1, vec![current_chunk_id]),
            (vec!["filter=literalneedle".to_string()], 1, vec![current_chunk_id]),
            (vec!["absentliteral".to_string()], 1, vec![]),
            (current_terms.clone(), 0, vec![]),
            (vec![], 1, vec![]),
        ] {
            let single = matching_store.list_chunks_by_revision_matching_terms(
                revision.id, &terms, limit,
            ).await?;
            let multi = matching_store.list_chunks_by_revisions_matching_terms(
                &[revision.id], &terms, limit,
            ).await?;
            assert_eq!(single.iter().map(|row| row.chunk_id).collect::<Vec<_>>(), expected);
            assert_eq!(multi.iter().map(|row| row.chunk_id).collect::<Vec<_>>(), expected);
            assert_eq!(serde_json::to_value(&single)?, serde_json::to_value(&multi)?);
        }
        assert!(matching_store.list_chunks_by_revisions_matching_terms(
            &[], &current_terms, 1,
        ).await?.is_empty());
        let restored: String = sqlx::query_scalar("select current_setting('plan_cache_mode')")
            .fetch_one(&matching_pool).await?;
        assert_eq!(restored, "force_generic_plan");
        let (generic, custom): (i64, i64) = sqlx::query_as(
            "select coalesce(sum(generic_plans),0)::bigint, coalesce(sum(custom_plans),0)::bigint
             from pg_prepared_statements
             where position('knowledge_chunk' in statement)>0 and position('strpos' in statement)>0",
        ).fetch_one(&matching_pool).await?;
        assert_eq!(generic, 1, "only SQLx describe may use the session generic policy");
        assert!(custom >= 6, "both matching entrypoints must execute custom plans");
        matching_pool.close().await;
        assert_eq!(
            store
                .list_head_source_unit_blocks_by_revision(revision.id, 1, None, None, false)
                .await?
                .len(),
            1
        );
        assert_eq!(
            store
                .search_code_pattern_chunks_by_terms(library.id, &[document.id], &code_terms, 1,)
                .await?
                .len(),
            0,
            "prepared pending candidates are not public code-search sources"
        );
        assert_eq!(
            store
                .search_transport_pattern_chunks_by_terms(
                    library.id,
                    &[document.id],
                    &transport_terms,
                    1,
                )
                .await?
                .len(),
            0,
            "prepared pending candidates are not public transport-search sources"
        );

        let mut publication = pool.begin().await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
            .bind(job.mutation_item_id.context("preparation job item")?)
            .execute(&mut *publication).await?;
        sqlx::query("update content_mutation set mutation_state='applied',completed_at=now() where id=$1")
            .bind(job.mutation_id.context("preparation job mutation")?)
            .execute(&mut *publication).await?;
        sqlx::query("update ingest_attempt set attempt_state='succeeded',finished_at=now() where id=$1")
            .bind(new_attempt.id).execute(&mut *publication).await?;
        sqlx::query("update ingest_job set queue_state='completed',completed_at=now() where id=$1")
            .bind(job.id).execute(&mut *publication).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2,latest_mutation_id=$3,latest_successful_attempt_id=$4 where document_id=$1")
            .bind(document.id).bind(revision.id)
            .bind(job.mutation_id).bind(new_attempt.id).execute(&mut *publication).await?;
        publication.commit().await?;
        let current_revision: Option<Uuid> = sqlx::query_scalar(
            "select revision_id from content_current_revision where document_id=$1",
        ).bind(document.id).fetch_optional(&pool).await?;
        assert_eq!(current_revision, Some(revision.id), "positive search control requires canonical publication");
        assert_eq!(store.search_code_pattern_chunks_by_terms(
            library.id, &[document.id], &code_terms, 1,
        ).await?.len(), 1);
        assert_eq!(store.search_transport_pattern_chunks_by_terms(
            library.id, &[document.id], &transport_terms, 1,
        ).await?.len(), 1);

        Ok(())
    })
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

/// IRG-175: the inline append materializer and the preparation fence must share
/// one checksum semantic — the SHA-256 of the extraction plan's normalized
/// text. `record_jsonl` is a non-identity adapter: its normalized text is a
/// re-rendered transcript, not the raw JSONL bytes. Hashing the raw merge in
/// the inline path stranded every valid append in `preparing` behind a 500
/// "extracted text changed before structured artifact finalization".
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn inline_record_jsonl_append_reaches_prepared_through_the_preparation_fence() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let transcript = concat!(
            "{\"id\":\"msg-1\",\"recordedAt\":\"2026-04-28T09:00:00Z\",\"speaker\":{\"role\":\"user\",\"label\":\"User One\"},\"text\":\"How do I configure search?\"}\n",
            "{\"id\":\"msg-2\",\"recordedAt\":\"2026-04-28T09:01:00Z\",\"speaker\":{\"role\":\"assistant\",\"label\":\"Assistant\"},\"text\":\"Use the library settings page.\"}\n",
        );
        let document = fixture
            .state
            .application_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("irg175-transcript-{}", Uuid::now_v7())),
                    file_name: Some("leader-gpt-transcript.jsonl".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create the record_jsonl transcript document")?;
        let document_id = document.id;
        let transcript_checksum = format!("sha256:{}", hex::encode(Sha256::digest(transcript.as_bytes())));
        let storage_key = fixture
            .state
            .content_storage
            .persist_revision_source(
                fixture.workspace_id,
                fixture.library_id,
                "leader-gpt-transcript.jsonl",
                &transcript_checksum,
                transcript.as_bytes(),
            )
            .await
            .context("failed to persist the record_jsonl transcript source bytes")?;
        let base_revision = crate::content_lifecycle_support::create_imported_revision(
                &fixture.state,
                CreateRevisionCommand {
                    document_id,
                    content_source_kind: "upload".to_string(),
                    checksum: transcript_checksum,
                    mime_type: "application/x-ndjson".to_string(),
                    byte_size: i64::try_from(transcript.len()).unwrap_or(i64::MAX),
                    title: Some("LeaderGPT transcript".to_string()),
                    language_code: None,
                    source_uri: Some("upload://leader-gpt-transcript.jsonl".to_string()),
                    document_hint: None,
                    storage_key: Some(storage_key),
                    created_by_principal_id: None,
                },
                "leader-gpt-transcript.jsonl",
            )
            .await
            .context("failed to create the record_jsonl transcript base revision")?;
        let base_revision_id = base_revision.id;
        // No worker runs in this fixture, so make the base revision readable by
        // hand. The stored source bytes the append path re-reads are already in
        // content storage under `storage_key`.
        fixture
            .state
            .application_services
            .knowledge
            .set_revision_extract_state(
                &fixture.state,
                base_revision_id,
                "ready",
                Some(transcript),
                Some("sha256:irg175-base"),
            )
            .await
            .context("failed to make the transcript base revision readable")?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    publishing_mutation_item_id: None,
                    document_id,
                    active_revision_id: Some(base_revision_id),
                    readable_revision_id: Some(base_revision_id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote the transcript base head")?;

        let appended_record = "{\"id\":\"msg-3\",\"recordedAt\":\"2026-04-28T09:02:00Z\",\"speaker\":{\"role\":\"user\",\"label\":\"User One\"},\"text\":\"Show me the ingestion status.\"}";
        // This fixture configures no EmbedChunk binding, so the append's
        // post-preparation embed stage fails after the fence has already
        // committed. IRG-175 is about the fence, so the assertion is on the
        // persisted artifact state: a fence failure strands the artifact in
        // `preparing`, a fence pass leaves it `prepared`.
        let _append_outcome = fixture
            .state
            .application_services
            .content
            .append_inline_mutation(
                &fixture.state,
                AppendInlineMutationCommand {
                    base_revision_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id,
                    idempotency_key: Some(format!("irg175-append-{}", Uuid::now_v7())),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    appended_text: appended_record.to_string(),
                },
            )
            .await;
        let appended_revision_id = fixture
            .state
            .application_services
            .content
            .list_revisions(&fixture.state, document_id)
            .await
            .context("failed to list revisions after the append")?
            .into_iter()
            .filter(|revision| revision.content_source_kind == "append")
            .max_by_key(|revision| revision.revision_number)
            .map(|revision| revision.id)
            .context("append did not create a result revision")?;

        let appended_revision = fixture
            .state
            .document_store
            .get_revision(appended_revision_id)
            .await?
            .context("missing appended revision")?;
        let structured = fixture
            .state
            .document_store
            .get_structured_revision(appended_revision_id)
            .await?
            .context("append did not persist a structured artifact")?;

        assert_eq!(structured.source_format, "record_jsonl");
        assert_eq!(
            structured.preparation_state, "prepared",
            "a valid inline record_jsonl append must finalize, not strand the revision in preparing",
        );
        assert_eq!(appended_revision.text_state, "text_readable");

        let normalized_text = appended_revision
            .normalized_text
            .clone()
            .context("a finalized append must store the normalized text")?;
        let canonical_checksum = hex::encode(Sha256::digest(normalized_text.as_bytes()));
        assert_eq!(
            appended_revision.text_checksum.as_deref(),
            Some(canonical_checksum.as_str()),
            "revision text_checksum must hash the stored normalized text",
        );

        // Prove the fixture exercised a non-identity adapter: the raw merge is
        // not the string the fence hashes.
        let merged_text = format!(
            "{}{}{}",
            transcript.trim_end_matches(['\r', '\n', ' ', '\t']),
            "\n",
            appended_record.trim_start_matches(['\r', '\n', ' ', '\t']),
        );
        assert_ne!(
            normalized_text, merged_text,
            "record_jsonl normalization must differ from the raw merged bytes for this proof to mean anything",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-175 negative control: the fence still rejects a checksum that does not
/// match the revision text it locks. Only the semantic was unified; the
/// comparison itself was neither deleted nor weakened.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn preparation_fence_rejects_a_checksum_that_differs_from_stored_revision_text() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("fence-drift-document-{}", Uuid::now_v7())),
                    file_name: Some("fence-drift.jsonl".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create the fence drift document")?;
        let revision = content
            .create_revision(
                &fixture.state,
                CreateRevisionCommand {
                    document_id: document.id,
                    content_source_kind: "upload".to_string(),
                    checksum: format!("sha256:fence-drift-{}", Uuid::now_v7()),
                    mime_type: "application/x-ndjson".to_string(),
                    byte_size: 64,
                    title: Some("Fence Drift".to_string()),
                    language_code: None,
                    source_uri: None,
                    document_hint: None,
                    storage_key: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create the fence drift revision")?;
        let pool = fixture.state.persistence.postgres.clone();
        let job = create_leased_content_mutation_job(
            &pool,
            ContentMutationRevisionScope {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: document.id,
                revision_id: revision.id,
            },
            format!("fence-drift-{}", Uuid::now_v7()),
            "fence-drift.jsonl",
        )
        .await?;
        let attempt =
            lease_content_mutation_attempt(&pool, job.id, 1, "fence-drift-lease", 30).await?;
        let store = PgDocumentStore { pool: pool.clone() };
        let authority =
            authority(fixture.workspace_id, fixture.library_id, attempt.id, revision.id);

        let extract = store
            .update_revision_extract_artifact_fenced(
                authority,
                "text_readable",
                IngestRevisionExtractArtifact::Replace {
                    normalized_text: "canonical normalized transcript",
                    text_checksum: "sha256:canonical-transcript",
                    text_readable_at: Some(Utc::now()),
                    image_checksum: None,
                },
            )
            .await?;
        assert!(matches!(extract, IngestAttemptMutationOutcome::Applied(_)));
        let now = Utc::now();
        let preparing = store
            .replace_structured_revision_artifact_fenced(
                authority,
                &KnowledgeStructuredRevisionRow {
                    revision_id: revision.id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    preparation_state: "preparing".to_string(),
                    normalization_profile: "fence-drift-v1".to_string(),
                    source_format: "record_jsonl".to_string(),
                    language_code: None,
                    block_count: 0,
                    chunk_count: 0,
                    typed_fact_count: 0,
                    outline_json: serde_json::json!([]),
                    prepared_at: now,
                    updated_at: now,
                },
                &[],
            )
            .await?;
        assert!(matches!(preparing, IngestAttemptMutationOutcome::Applied(_)));

        let drift = store
            .finalize_structured_revision_artifact_fenced(authority, "sha256:raw-merged-jsonl")
            .await
            .expect_err(
                "a checksum that differs from the stored revision text must fail the fence",
            );
        assert!(
            drift
                .to_string()
                .contains("extracted text changed before structured artifact finalization"),
            "the fence must reject drift with its explicit message, got: {drift}",
        );

        let finalized = store
            .finalize_structured_revision_artifact_fenced(authority, "sha256:canonical-transcript")
            .await?;
        match finalized {
            IngestAttemptMutationOutcome::Applied(row) => {
                assert_eq!(row.preparation_state, "prepared");
            }
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Err(anyhow!("matching checksum finalization lost its authority"));
            }
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
