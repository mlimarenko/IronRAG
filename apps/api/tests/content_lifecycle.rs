#[path = "support/content_lifecycle_support.rs"]
mod content_lifecycle_support;
#[cfg(feature = "test-support")]
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::{Context, Result};
use uuid::Uuid;

use ironrag_backend::{
    infra::repositories::{self, iam_repository},
    services::{
        content::service::{
            AcceptMutationCommand, AdmitMutationCommand, AppendInlineMutationCommand,
            CreateDocumentCommand, EditInlineMutationCommand, PromoteHeadCommand,
            ReplaceInlineMutationCommand, RevisionAdmissionMetadata, UploadInlineDocumentCommand,
        },
        knowledge::service::PromoteKnowledgeDocumentCommand,
    },
};

#[cfg(feature = "test-support")]
use ironrag_backend::services::ingest::web::CreateWebIngestRunCommand;

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_document_creation_rolls_back_when_projection_insert_fails() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let external_key = format!("projection-conflict-{}", Uuid::now_v7());
        let orphan_projection_id = Uuid::now_v7();
        sqlx::query(
            "insert into knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role, created_at, updated_at, deleted_at
             ) values ($1, $2, $3, $4, $4, null, 'active', null, null, null,
                       null, 'primary', now(), now(), null)",
        )
        .bind(orphan_projection_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(&external_key)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to seed conflicting orphan projection")?;

        fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(external_key.clone()),
                    file_name: Some("ignored-input-name.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .expect_err("a failed projection insert must fail the canonical create transaction");

        let canonical_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from content_document
             where library_id = $1 and external_key = $2",
        )
        .bind(fixture.library_id)
        .bind(&external_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(
            canonical_count, 0,
            "projection failure must roll back the canonical document and head",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_revision_creation_self_heals_or_rolls_back_as_one_transaction() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("revision-rollback-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create revision rollback document")?;
        sqlx::query("delete from knowledge_document where document_id = $1")
            .bind(document.id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to remove revision rollback projection")?;

        let first_revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:projection-rollback",
                    "Projection rollback",
                    Some("upload://projection-rollback.txt"),
                ),
            )
            .await
            .context("missing document projection must self-heal inside revision creation")?;

        let healed_document = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("revision creation did not self-heal the document projection")?;
        let healed_revision =
            fixture
                .state
                .document_store
                .get_revision(first_revision.id)
                .await?
                .context("revision creation did not atomically create its projection")?;
        assert_eq!(healed_document.document_id, document.id);
        assert_eq!(healed_revision.revision_id, first_revision.id);

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table knowledge_revision
             add constraint knowledge_revision_test_title_block
             check (
                document_id <> '{}'::uuid
                or title is distinct from 'Blocked revision projection'
             )",
            document.id,
        )))
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to install revision projection failure constraint")?;
        let blocked_result = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:blocked-revision-projection",
                    "Blocked revision projection",
                    Some("upload://blocked-revision-projection.txt"),
                ),
            )
            .await;
        sqlx::query(
            "alter table knowledge_revision
             drop constraint knowledge_revision_test_title_block",
        )
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to remove revision projection failure constraint")?;
        blocked_result
            .err()
            .context("projection failure must reject the complete revision transaction")?;

        let canonical_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from content_revision where document_id = $1",
        )
        .bind(document.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        let projection_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_revision where document_id = $1",
        )
        .bind(document.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(canonical_count, 1, "failed second revision must roll back canonically");
        assert_eq!(projection_count, canonical_count, "revision planes must retain parity");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn materializer_inserts_missing_projection_and_uses_only_canonical_metadata() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let external_key = format!("canonical-metadata-{}", Uuid::now_v7());
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(external_key.clone()),
                    file_name: Some("canonical-input.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create materializer document")?;
        let initial_projection = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("initial materializer projection missing")?;
        assert_eq!(
            initial_projection.file_name.as_deref(),
            Some("canonical-input.txt"),
            "the admitted safe file name must be persisted on the canonical-backed projection",
        );

        sqlx::query("delete from knowledge_document where document_id = $1")
            .bind(document.id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to remove document projection")?;
        let outcome =
            repositories::content_repository::materialize_knowledge_document_from_canonical_head(
                &fixture.state.persistence.postgres,
                document.id,
            )
            .await
            .context("materializer failed to recreate a missing projection")?;
        assert_eq!(
            outcome,
            repositories::content_repository::MaterializeKnowledgeDocumentOutcome::Materialized,
        );
        let recreated = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("materializer did not recreate the document projection")?;
        assert_eq!(recreated.external_key, external_key);
        assert_eq!(recreated.file_name.as_deref(), Some("canonical-input.txt"));
        assert_eq!(recreated.title, None);

        let mut command = revision_command(
            document.id,
            "upload",
            "sha256:canonical-metadata",
            "temporary title",
            None,
        );
        command.title = None;
        command.storage_key = Some(format!(
            "content/{}/{}/{}-canonical-report.pdf",
            fixture.workspace_id,
            fixture.library_id,
            "a".repeat(64),
        ));
        let revision = content
            .create_revision(&fixture.state, command)
            .await
            .context("failed to create canonical metadata revision")?;
        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to establish canonical metadata head")?;
        sqlx::query(
            "update knowledge_document
             set title = 'stale title', file_name = 'stale-name.bin'
             where document_id = $1",
        )
        .bind(document.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        repositories::content_repository::materialize_knowledge_document_from_canonical_head(
            &fixture.state.persistence.postgres,
            document.id,
        )
        .await
        .context("failed to rematerialize canonical metadata")?;
        let rematerialized = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("rematerialized document projection missing")?;
        assert_eq!(
            rematerialized.title, None,
            "a canonical NULL title must clear stale projected title",
        );
        assert_eq!(
            rematerialized.file_name.as_deref(),
            Some("canonical-report.pdf"),
            "file name must be deterministically reconstructed from canonical storage metadata",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn failed_head_projection_rolls_back_the_canonical_head() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("head-rollback-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create head rollback document")?;
        let revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:head-rollback",
                    "Blocked projection title",
                    Some("upload://head-rollback.txt"),
                ),
            )
            .await
            .context("failed to create head rollback revision")?;
        // Every fixture owns an isolated temporary database. Scope the
        // failpoint to this document anyway, and remove it before evaluating
        // the result so even an assertion/error path cannot leak DDL into the
        // remainder of the fixture.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table knowledge_document
             add constraint knowledge_document_test_title_block
             check (
                document_id <> '{}'::uuid
                or title is distinct from 'Blocked projection title'
             )",
            document.id,
        )))
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to install projection failure constraint")?;

        let promotion_result = content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await;
        sqlx::query(
            "alter table knowledge_document
             drop constraint knowledge_document_test_title_block",
        )
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to remove projection failure constraint")?;
        promotion_result
            .err()
            .context("projection failure must fail the complete head transaction")?;

        let head = repositories::content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document.id,
        )
        .await?
        .context("head shell disappeared after rolled-back promotion")?;
        assert_eq!(head.active_revision_id, None);
        assert_eq!(head.readable_revision_id, None);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn chunk_projection_failure_rolls_back_both_chunk_planes() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("chunk-atomic-{}", Uuid::now_v7())),
                    file_name: Some("chunk-atomic.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create chunk atomic document")?;
        let revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:chunk-atomic",
                    "Chunk atomic",
                    Some("upload://chunk-atomic.txt"),
                ),
            )
            .await
            .context("failed to create chunk atomic revision")?;
        let original = repositories::content_repository::NewContentChunkProjection {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: document.id,
            revision_id: revision.id,
            chunk_index: 0,
            start_offset: 0,
            end_offset: 16,
            token_count: Some(3),
            chunk_kind: Some("paragraph".to_string()),
            content_text: "original evidence".to_string(),
            normalized_text: "original evidence".to_string(),
            text_checksum:
                "sha256:37a084be87246ef17563f0337770335bf02cd3d9cccb623b02e6b70d973ef74b"
                    .to_string(),
            support_block_ids: Vec::new(),
            section_path: vec!["Overview".to_string()],
            heading_trail: vec!["Overview".to_string()],
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(i64::from(revision.revision_number)),
            vector_generation: None,
            quality_score: Some(1.0),
            window_text: Some("original evidence".to_string()),
            occurred_at: None,
            occurred_until: None,
        };
        repositories::content_repository::replace_chunks_with_projection(
            &fixture.state.persistence.postgres,
            revision.id,
            std::slice::from_ref(&original),
        )
        .await
        .context("failed to establish atomic chunk baseline")?;
        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote chunk atomic baseline")?;

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table knowledge_chunk
             add constraint knowledge_chunk_test_atomic_block
             check (
                revision_id <> '{}'::uuid
                or content_text <> 'blocked replacement'
             )",
            revision.id,
        )))
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to install chunk projection failure constraint")?;
        let mut blocked = original.clone();
        blocked.end_offset = 19;
        blocked.content_text = "blocked replacement".to_string();
        blocked.normalized_text = "blocked replacement".to_string();
        blocked.text_checksum =
            "sha256:1f27795bce9e989c841cfd0d7f3a6eba5769627f789fbbbbf19340b9d73f2fa5".to_string();
        blocked.window_text = Some("blocked replacement".to_string());
        let replacement_result = repositories::content_repository::replace_chunks_with_projection(
            &fixture.state.persistence.postgres,
            revision.id,
            &[blocked],
        )
        .await;
        sqlx::query(
            "alter table knowledge_chunk
             drop constraint knowledge_chunk_test_atomic_block",
        )
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to remove chunk projection failure constraint")?;
        replacement_result
            .err()
            .context("knowledge chunk failure must reject the complete replacement")?;

        let canonical = repositories::content_repository::list_chunks_by_revision(
            &fixture.state.persistence.postgres,
            revision.id,
        )
        .await?;
        let projected = fixture.state.document_store.list_chunks_by_revision(revision.id).await?;
        assert_eq!(canonical.len(), 1);
        assert_eq!(projected.len(), 1);
        assert_eq!(canonical[0].normalized_text, "original evidence");
        assert_eq!(projected[0].content_text, "original evidence");
        assert_eq!(canonical[0].id, projected[0].chunk_id);
        let fingerprint =
            repositories::content_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            fingerprint.projection_is_current,
            "rolled-back replacement must retain canonical/document/revision/chunk parity",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn image_attachment_promotion_keeps_canonical_and_knowledge_roles_in_sync() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let pool = &fixture.state.persistence.postgres;
        let parent_key = format!("role-parent-{}", Uuid::now_v7());
        let parent = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(parent_key.clone()),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create role-sync parent")?;
        let image = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    // Keep the canonical external key intentionally opaque:
                    // media classification must use the persisted file name.
                    external_key: Some(format!("role-image-{}", Uuid::now_v7())),
                    file_name: Some("diagram.png".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: Some(parent_key),
                },
            )
            .await
            .context("failed to create role-sync image attachment")?;
        let canonical_before = repositories::content_repository::get_document_by_id(pool, image.id)
            .await?
            .context("canonical image attachment missing before promotion")?;
        assert_eq!(canonical_before.parent_document_id, Some(parent.id));
        assert_eq!(canonical_before.document_role, "attachment");

        let mut image_revision_command = revision_command(
            image.id,
            "upload",
            "sha256:role-image",
            "Role image",
            Some("upload://role-image"),
        );
        image_revision_command.mime_type = "image/png".to_string();
        let revision = content
            .create_revision(&fixture.state, image_revision_command)
            .await
            .context("failed to create role-sync image revision")?;

        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: image.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote role-sync image attachment")?;

        let canonical = repositories::content_repository::get_document_by_id(pool, image.id)
            .await?
            .context("canonical image attachment missing after promotion")?;
        let knowledge = fixture
            .state
            .document_store
            .get_document(image.id)
            .await?
            .context("knowledge image attachment missing after promotion")?;
        assert_eq!(canonical.document_role, "attached_context");
        assert_eq!(knowledge.document_role, canonical.document_role);

        let fingerprint =
            repositories::content_repository::get_library_readable_content_fingerprint(
                pool,
                fixture.library_id,
            )
            .await?;
        assert!(
            fingerprint.projection_is_current,
            "a successful promotion must not leave document-role projection drift",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn delayed_older_promotion_cannot_overwrite_superseding_canonical_head() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("promotion-race-{}", Uuid::now_v7())),
                    file_name: Some("promotion-race.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create promotion-race document")?;
        let older_revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:promotion-race-older",
                    "Older revision",
                    Some("upload://promotion-race-older"),
                ),
            )
            .await
            .context("failed to create older promotion-race revision")?;
        let newer_revision = content
            .append_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:promotion-race-newer",
                    "Newer revision",
                    Some("upload://promotion-race-newer"),
                ),
            )
            .await
            .context("failed to create newer promotion-race revision")?;

        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(older_revision.id),
                    readable_revision_id: Some(older_revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to establish older promotion-race head")?;

        let document_id = document.id;
        let newer_revision_id = newer_revision.id;

        // Hold B's parent-first lifecycle lock while the delayed projection
        // phase of A starts. B then advances the canonical head inside that
        // transaction. A must serialize on the parent, observe B after commit,
        // and materialize B rather than its stale command payload.
        let mut superseding_transaction = fixture.state.persistence.postgres.begin().await?;
        assert!(
            repositories::catalog_repository::lock_library_for_lifecycle_event_with_executor(
                &mut *superseding_transaction,
                fixture.workspace_id,
                fixture.library_id,
            )
            .await?,
            "promotion-race library must exist",
        );
        let delayed_state = fixture.state.clone();
        let start_barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let delayed_barrier = start_barrier.clone();
        let delayed_older_promotion = tokio::spawn(async move {
            delayed_barrier.wait().await;
            delayed_state
                .canonical_services
                .knowledge
                .promote_document(&delayed_state, PromoteKnowledgeDocumentCommand { document_id })
                .await
                .map_err(|error| anyhow::anyhow!("delayed older promotion failed: {error}"))
        });
        start_barrier.wait().await;
        tokio::task::yield_now().await;
        repositories::content_repository::upsert_document_head_with_executor(
            &mut *superseding_transaction,
            &repositories::content_repository::NewContentDocumentHead {
                document_id,
                active_revision_id: Some(newer_revision_id),
                readable_revision_id: Some(newer_revision_id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to write superseding canonical promotion-race head")?;
        superseding_transaction
            .commit()
            .await
            .context("failed to commit superseding promotion-race transaction")?;
        tokio::time::timeout(std::time::Duration::from_secs(10), delayed_older_promotion)
            .await
            .context("delayed promotion did not resume after superseding commit")?
            .context("delayed promotion task panicked")??;

        let canonical_head = repositories::content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?
        .context("canonical promotion-race head missing")?;
        let knowledge = fixture
            .state
            .document_store
            .get_document(document_id)
            .await?
            .context("knowledge promotion-race document missing")?;
        assert_eq!(canonical_head.active_revision_id, Some(newer_revision_id));
        assert_eq!(canonical_head.readable_revision_id, Some(newer_revision_id));
        assert_eq!(knowledge.active_revision_id, canonical_head.active_revision_id);
        assert_eq!(knowledge.readable_revision_id, canonical_head.readable_revision_id);
        assert_eq!(knowledge.latest_revision_no, Some(2));
        assert_eq!(knowledge.title.as_deref(), Some("Newer revision"));

        let fingerprint =
            repositories::content_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            fingerprint.projection_is_current,
            "an out-of-order stale promotion must converge to the superseding canonical head",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn delete_parent_cascades_attached_context_and_detaches_attachment_children() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let pool = &fixture.state.persistence.postgres;
        let parent_key = format!("parent-page-{}", Uuid::now_v7());

        // Parent document.
        let parent = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(parent_key.clone()),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create parent document")?;

        // Two children declaring the parent: admission resolves parent_document_id.
        let image_child = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("image-child-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: Some(parent_key.clone()),
                },
            )
            .await
            .context("failed to create image child document")?;
        let attachment_child = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("doc-child-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: Some(parent_key.clone()),
                },
            )
            .await
            .context("failed to create attachment child document")?;

        // Both children resolved the parent at admission (provisional role).
        for child_id in [image_child.id, attachment_child.id] {
            let row = repositories::content_repository::get_document_by_id(pool, child_id)
                .await?
                .context("child row missing after admission")?;
            assert_eq!(row.parent_document_id, Some(parent.id));
            assert_eq!(row.parent_external_key.as_deref(), Some(parent_key.as_str()));
        }

        // Finalize roles as the promote path would: the image child is
        // subordinate attached context; the other stays a peer attachment.
        sqlx::query("update content_document set document_role = 'attached_context' where id = $1")
            .bind(image_child.id)
            .execute(pool)
            .await
            .context("failed to set attached_context role")?;
        sqlx::query("update content_document set document_role = 'attachment' where id = $1")
            .bind(attachment_child.id)
            .execute(pool)
            .await
            .context("failed to set attachment role")?;

        // Delete the parent.
        content
            .delete_document(&fixture.state, parent.id)
            .await
            .context("failed to delete parent document")?;

        // attached_context child is cascade-soft-deleted.
        let image_row = repositories::content_repository::get_document_by_id(pool, image_child.id)
            .await?
            .context("image child row missing after cascade")?;
        assert_eq!(
            image_row.document_state, "deleted",
            "attached_context child must be cascade-soft-deleted with its parent"
        );

        // attachment child survives, detached but keeps its declared key.
        let attachment_row =
            repositories::content_repository::get_document_by_id(pool, attachment_child.id)
                .await?
                .context("attachment child row missing after cascade")?;
        assert_ne!(
            attachment_row.document_state, "deleted",
            "attachment child must survive the parent delete"
        );
        assert_eq!(
            attachment_row.parent_document_id, None,
            "attachment child must be detached from the deleted parent"
        );
        assert_eq!(
            attachment_row.parent_external_key.as_deref(),
            Some(parent_key.as_str()),
            "detached attachment child must keep its declared parent key for re-resolution"
        );
        let attachment_projection = fixture
            .state
            .document_store
            .get_document(attachment_child.id)
            .await?
            .context("attachment knowledge projection missing after parent delete")?;
        assert_eq!(
            attachment_projection.parent_document_id, None,
            "retrieval projection must detach the surviving attachment atomically",
        );

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
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
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
        repositories::content_repository::create_mutation_item(
            &fixture.state.persistence.postgres,
            &repositories::content_repository::NewContentMutationItem {
                mutation_id: mutation.id,
                document_id: Some(document.id),
                base_revision_id: Some(readable_revision.id),
                result_revision_id: Some(active_revision.id),
                item_state: "pending",
                message: Some("head-parentage regression fixture"),
            },
        )
        .await
        .context("failed to anchor append mutation to its document")?;

        let source_truth_before = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;

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
        let source_truth_after = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        assert!(
            source_truth_after > source_truth_before,
            "a committed head promotion must invalidate cross-replica answer cache identities"
        );

        let knowledge_document = fixture
            .state
            .document_store
            .get_document(document.id)
            .await
            .context("failed to load promoted knowledge document")?
            .context("missing promoted knowledge document")?;
        assert_eq!(knowledge_document.document_state, "active");
        assert_eq!(knowledge_document.active_revision_id, Some(active_revision.id));
        assert_eq!(knowledge_document.readable_revision_id, Some(readable_revision.id));
        assert_ne!(knowledge_document.readable_revision_id, knowledge_document.active_revision_id);

        sqlx::query(
            "update knowledge_document
             set readable_revision_id = $2
             where document_id = $1",
        )
        .bind(document.id)
        .bind(active_revision.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let divergent_identity =
            repositories::content_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            !divergent_identity.projection_is_current,
            "a stale readable projection must disable answer replay and generation"
        );
        sqlx::query(
            "update knowledge_document
             set readable_revision_id = $2
             where document_id = $1",
        )
        .bind(document.id)
        .bind(readable_revision.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        repositories::catalog_repository::touch_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let converged_identity =
            repositories::content_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(converged_identity.projection_is_current);

        fixture
            .state
            .canonical_services
            .content
            .update_active_revision_document_hint(
                &fixture.state,
                document.id,
                Some("Updated neutral source hint".to_string()),
            )
            .await
            .context("failed to update readable revision document hint")?;
        let source_truth_after_hint = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        assert!(
            source_truth_after_hint > source_truth_after,
            "a readable document-hint update must invalidate cross-replica answer cache identities"
        );

        let canonical_before_missing_projection =
            repositories::content_repository::get_revision_by_id(
                &fixture.state.persistence.postgres,
                readable_revision.id,
            )
            .await?
            .context("canonical readable revision missing before rollback regression")?;
        let generation_before_missing_projection = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        sqlx::query("delete from knowledge_revision where revision_id = $1")
            .bind(readable_revision.id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to remove knowledge revision for rollback regression")?;

        let update_error = fixture
            .state
            .canonical_services
            .content
            .update_active_revision_document_hint(
                &fixture.state,
                document.id,
                Some("This update must roll back".to_string()),
            )
            .await
            .expect_err("missing knowledge projection must fail closed");
        assert!(
            matches!(
                update_error,
                ironrag_backend::interfaces::http::router_support::ApiError::ServiceUnavailable {
                    kind: "document_projection_converging",
                    ..
                }
            ),
            "missing knowledge revision must surface as a retryable 503 projection error",
        );
        let canonical_after_missing_projection =
            repositories::content_repository::get_revision_by_id(
                &fixture.state.persistence.postgres,
                readable_revision.id,
            )
            .await?
            .context("canonical readable revision disappeared after rollback regression")?;
        assert_eq!(
            canonical_after_missing_projection.document_hint,
            canonical_before_missing_projection.document_hint,
            "the canonical hint must roll back with the missing projection",
        );
        assert_eq!(
            repositories::get_library_source_truth_version(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?,
            generation_before_missing_projection,
            "a rolled-back projection update must not advance source truth",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_edit_mutation_persists_source_for_reprocess() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let edited_markdown =
            "## Sheet1\n\n| Item | Quantity |\n| --- | --- |\n| Widget | 9 |".to_string();
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Content Lifecycle Edit Principal",
            None,
        )
        .await
        .context("failed to create content lifecycle edit principal")?;
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("edit-doc-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create edit lifecycle document")?;
        let base_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:edit-base",
                    "Inventory.xlsx",
                    Some("upload://inventory.xlsx"),
                ),
            )
            .await
            .context("failed to create edit base revision")?;
        fixture
            .state
            .canonical_services
            .knowledge
            .set_revision_extract_state(
                &fixture.state,
                base_revision.id,
                "ready",
                Some("| Item | Quantity |\n| --- | --- |\n| Widget | 7 |"),
                Some("sha256:edit-base-text"),
            )
            .await
            .context("failed to persist readable extract for edit base revision")?;
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
            .context("failed to promote edit base head")?;

        let edit_admission = fixture
            .state
            .canonical_services
            .content
            .edit_inline_mutation(
                &fixture.state,
                EditInlineMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some("canonical-edit".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    markdown: edited_markdown.clone(),
                },
            )
            .await
            .context("failed to admit canonical edit mutation")?;

        assert_eq!(edit_admission.mutation.mutation_state, "accepted");
        let edit_revision_id = edit_admission
            .items
            .first()
            .and_then(|item| item.result_revision_id)
            .context("edit admission must create a revision")?;
        let edit_revision = fixture
            .state
            .canonical_services
            .content
            .list_revisions(&fixture.state, document.id)
            .await
            .context("failed to list revisions after edit admission")?
            .into_iter()
            .find(|revision| revision.id == edit_revision_id)
            .context("edited revision missing from revision list")?;
        assert_eq!(edit_revision.content_source_kind, "edit");
        assert_eq!(edit_revision.mime_type, "text/markdown");
        let storage_key = edit_revision.storage_key.clone().context(
            "edited revision must persist a stored markdown source for future reprocess",
        )?;
        let stored_bytes = fixture
            .state
            .content_storage
            .read_revision_source(&storage_key)
            .await
            .context("failed to read stored edit markdown source")?;
        assert_eq!(
            String::from_utf8(stored_bytes).context("edited source must remain utf-8 markdown")?,
            edited_markdown
        );
        let resolved_storage_key = fixture
            .state
            .canonical_services
            .content
            .resolve_revision_storage_key(&fixture.state, edit_revision.id)
            .await
            .context("failed to resolve stored key for edited revision")?;
        assert_eq!(resolved_storage_key.as_deref(), Some(storage_key.as_str()));

        let repeated_edit_admission = fixture
            .state
            .canonical_services
            .content
            .edit_inline_mutation(
                &fixture.state,
                EditInlineMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some("canonical-edit".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    markdown: edited_markdown.clone(),
                },
            )
            .await
            .context("failed to replay canonical edit mutation")?;
        assert_eq!(repeated_edit_admission.mutation.id, edit_admission.mutation.id);
        assert_eq!(repeated_edit_admission.job_id, edit_admission.job_id);
        assert_eq!(
            repeated_edit_admission.items.first().and_then(|item| item.result_revision_id),
            Some(edit_revision.id)
        );

        fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(edit_revision.id),
                    readable_revision_id: Some(base_revision.id),
                    latest_mutation_id: Some(edit_admission.mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote edited revision as active head")?;

        let reprocess_admission = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "reprocess".to_string(),
                    idempotency_key: Some("canonical-edit-reprocess".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: Some(RevisionAdmissionMetadata {
                        content_source_kind: edit_revision.content_source_kind.clone(),
                        checksum: edit_revision.checksum.clone(),
                        mime_type: edit_revision.mime_type.clone(),
                        byte_size: edit_revision.byte_size,
                        title: edit_revision.title.clone(),
                        language_code: edit_revision.language_code.clone(),
                        source_uri: edit_revision.source_uri.clone(),
                        document_hint: edit_revision.document_hint.clone(),
                        storage_key: Some(storage_key),
                    }),
                    parent_async_operation_id: None,
                },
            )
            .await
            .context("failed to admit reprocess for edited revision")?;

        assert_eq!(reprocess_admission.mutation.mutation_state, "accepted");
        assert!(reprocess_admission.job_id.is_some());

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
                    document_hint: None,
                    mime_type: Some("text/plain".to_string()),
                    file_bytes: b"Ada Lovelace wrote the note.\nCharles Babbage built the engine."
                        .to_vec(),
                    parent_external_key: None,
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
            .document_store
            .get_revision(revision_id)
            .await
            .context("failed to load admitted inline upload revision")?
            .context("missing admitted inline upload revision")?;

        let postgres_chunks =
            ironrag_backend::infra::repositories::content_repository::list_chunks_by_revision(
                &fixture.state.persistence.postgres,
                revision_id,
            )
            .await
            .context("failed to list postgres chunks for inline upload")?;
        let knowledge_chunks = fixture
            .state
            .document_store
            .list_chunks_by_revision(revision_id)
            .await
            .context("failed to list knowledge chunks for inline upload")?;
        let ingest_jobs = ironrag_backend::infra::repositories::ingest_repository::list_ingest_jobs_by_mutation_ids(
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

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_single_page_web_ingest_materializes_only_the_seed_page()
-> Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;
    web_ingest_support::enable_loopback_test_transport(&mut fixture.state);
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let seed_url = server.url("/seed");
        let web_policy = ironrag_backend::shared::web::ingest::default_web_ingest_policy();
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
                    crawl_filter: web_policy.crawl_filter,
                    materialization_filter: web_policy.materialization_filter,
                    requested_by_principal_id: None,
                    request_surface: "test".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit single-page web ingest run")?;

        assert_eq!(run.mode, "single_page");
        assert_eq!(run.run_state, "processing");

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

    let result = Box::pin(async {
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
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
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
                    document_hint: None,
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
                    document_hint: None,
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
        let referenced_chunk_id = Uuid::now_v7();
        let query_conversation_id = Uuid::now_v7();
        let query_execution_id = Uuid::now_v7();
        sqlx::query(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum
             ) values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(referenced_chunk_id)
        .bind(base_revision.id)
        .bind(0_i32)
        .bind(0_i32)
        .bind(32_i32)
        .bind(8_i32)
        .bind("base revision content for query ref")
        .bind("sha256:content-lifecycle-query-ref")
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to insert base chunk referenced by query history")?;
        sqlx::query(
            "insert into query_conversation (id, workspace_id, library_id, title)
             values ($1, $2, $3, $4)",
        )
        .bind(query_conversation_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind("Delete cleanup regression conversation")
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to insert query conversation for delete cleanup regression")?;
        sqlx::query(
            "insert into query_execution (
                id, workspace_id, library_id, conversation_id, context_bundle_id, query_text
             ) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(query_execution_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(query_conversation_id)
        .bind(Uuid::now_v7())
        .bind("Which facts came from the base revision?")
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to insert query execution for delete cleanup regression")?;
        sqlx::query(
            "insert into query_chunk_reference (execution_id, chunk_id, rank, score)
             values ($1, $2, $3, $4)",
        )
        .bind(query_execution_id)
        .bind(referenced_chunk_id)
        .bind(1_i32)
        .bind(0.91_f64)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to insert query chunk reference for delete cleanup regression")?;
        let query_reference_count_before_delete = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from query_chunk_reference where chunk_id = $1",
        )
        .bind(referenced_chunk_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count query chunk references before delete")?;
        assert_eq!(query_reference_count_before_delete, 1);

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
                    parent_async_operation_id: None,
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
        assert_eq!(delete_operation.status.as_str(), "ready");

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
        assert_eq!(replace_async_operation.status.as_str(), "failed");
        assert_eq!(replace_async_operation.failure_code.as_deref(), Some("document_deleted"));
        let replace_job_handle_after_delete = fixture
            .state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(&fixture.state, replace_mutation_id)
            .await
            .context("failed to reload superseded replace ingest job after delete")?
            .context("superseded replace mutation must retain its ingest job handle")?;
        assert_eq!(
            replace_job_handle_after_delete.job.queue_state, "canceled",
            "delete must retire queued superseded ingest work immediately"
        );

        let knowledge_document = fixture
            .state
            .document_store
            .get_document(document.id)
            .await
            .context("failed to reload deleted knowledge document")?
            .context("deleted knowledge document missing from store")?;
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
        let deleted_summary = fixture
            .state
            .canonical_services
            .content
            .get_document(&fixture.state, document.id)
            .await
            .context("failed to load deleted document summary")?;
        assert_eq!(deleted_summary.document.document_state, "deleted");
        assert!(deleted_summary.active_revision.is_none());
        assert!(
            deleted_summary.readiness.is_none(),
            "deleted document detail must not expose stale readiness state"
        );
        assert!(
            deleted_summary.readiness_summary.is_none(),
            "deleted document detail must not expose stale readiness summary"
        );
        assert!(
            deleted_summary.prepared_revision.is_none(),
            "deleted document detail must not expose stale prepared revision"
        );
        assert!(
            deleted_summary.source_access.is_none(),
            "deleted document detail must not expose source download access"
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
        let query_reference_count_after_delete = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from query_chunk_reference where chunk_id = $1",
        )
        .bind(referenced_chunk_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count query chunk references after delete")?;
        assert_eq!(
            query_reference_count_after_delete, 0,
            "delete must clear query chunk references contributed by the deleted document"
        );

        let repaired_projection = fixture
            .state
            .document_store
            .update_document_pointers(
                document.id,
                "active",
                Some(base_revision.id),
                Some(base_revision.id),
                Some(i64::from(base_revision.revision_number)),
                Some("base.txt"),
                None,
            )
            .await
            .context("failed to force stale active knowledge projection before repeated delete")?
            .context("forced stale active knowledge projection missing")?;
        assert_eq!(repaired_projection.document_state, "active");
        let leaked_documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after forcing stale active knowledge projection")?;
        assert!(
            leaked_documents.iter().any(|summary| summary.document.id == document.id),
            "stale active knowledge projection must reproduce the leaked deleted document before repair"
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
                    parent_async_operation_id: None,
                },
            )
            .await;
        let repeated_delete =
            repeated_delete.context("failed to replay canonical delete mutation")?;
        assert_eq!(repeated_delete.mutation.id, delete_mutation_id);
        assert_eq!(repeated_delete.mutation.mutation_state, "applied");
        let healed_knowledge_document = fixture
            .state
            .document_store
            .get_document(document.id)
            .await
            .context("failed to reload healed knowledge document after repeated delete")?
            .context("healed knowledge document missing from store")?;
        assert_eq!(healed_knowledge_document.document_state, "deleted");
        assert_eq!(healed_knowledge_document.active_revision_id, None);
        assert_eq!(healed_knowledge_document.readable_revision_id, Some(base_revision.id));
        assert!(healed_knowledge_document.deleted_at.is_some());
        let delete_outbox_count: i64 = sqlx::query_scalar(
            "select count(*)
             from webhook_lifecycle_outbox
             where event_type = 'document.deleted'
               and payload_json ->> 'document_id' = $1",
        )
        .bind(document.id.to_string())
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count durable document delete events")?;
        assert_eq!(
            delete_outbox_count, 1,
            "repeated delete must reuse the one persisted lifecycle transition",
        );
        let active_documents_after_repeated_delete = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list active documents after repeated delete repair")?;
        assert!(
            active_documents_after_repeated_delete
                .iter()
                .all(|summary| summary.document.id != document.id),
            "repeated delete must heal stale knowledge projections and hide the deleted document again"
        );

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
    })
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_delete_succeeds_when_post_commit_cleanup_fails() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Content Delete Cleanup Principal",
            None,
        )
        .await
        .context("failed to create delete cleanup principal")?;
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("delete-cleanup-doc-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create delete cleanup document")?;
        let base_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:delete-cleanup-base",
                    "Delete Cleanup Base Revision",
                    Some("file:///delete-cleanup.txt"),
                ),
            )
            .await
            .context("failed to create delete cleanup revision")?;
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
            .context("failed to promote delete cleanup head")?;

        sqlx::query("drop table query_chunk_reference")
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to drop query_chunk_reference for delete cleanup regression")?;

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
                    idempotency_key: Some("delete-post-commit-cleanup-failure".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .context("delete must succeed even if post-commit cleanup fails")?;
        assert_eq!(delete_admission.mutation.mutation_state, "applied");

        let deleted_document = fixture
            .state
            .canonical_services
            .content
            .get_document(&fixture.state, document.id)
            .await
            .context("failed to load deleted document after cleanup failure")?;
        assert_eq!(deleted_document.document.document_state, "deleted");
        assert!(deleted_document.active_revision.is_none());

        let knowledge_document = fixture
            .state
            .document_store
            .get_document(document.id)
            .await
            .context("failed to load knowledge document after cleanup failure")?
            .context("knowledge document missing after cleanup failure")?;
        assert_eq!(knowledge_document.document_state, "deleted");

        let active_documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list active documents after cleanup failure")?;
        assert!(
            active_documents.iter().all(|summary| summary.document.id != document.id),
            "deleted document must stay hidden even if post-commit cleanup fails"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Regression: deleting the last document supporting a graph entity must drop
/// the entity from the runtime graph projection — including stranded debris
/// from earlier deletes whose graph cleanup never converged.
///
/// Reproduces the canonical-cleanup bug observed on the prod Wiki library:
/// 27 nodes / 25 edges survived after every backing document was soft-deleted,
/// because evidence rows for previously-deleted documents kept inflating
/// `support_count` via the unfiltered recalculation. Asserts the canonical
/// fix on three fronts:
///
/// 1. Library-wide orphan sweep: evidence rows pointing at any
///    `document_state = 'deleted'` document are pruned during the next
///    delete, not just rows for the explicit doc.
/// 2. Active-document filter on `support_count`: orphan evidence does not
///    keep a node alive even before the orphan sweep runs.
/// 3. `runtime_graph_canonical_summary` rows for pruned targets are
///    removed (no FK cascade exists on that table).
#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_delete_drops_orphan_runtime_graph_state() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Graph Cleanup Principal",
            None,
        )
        .await
        .context("failed to create graph cleanup principal")?;

        // Two documents both supporting the same entity. `stranded` simulates
        // the prod scenario: a document that was already soft-deleted by a
        // prior cycle whose evidence sweep never ran. `current` is the doc
        // we delete in this test — its delete must clean up both itself AND
        // the stranded debris.
        let stranded_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("graph-stranded-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create stranded graph cleanup document")?;
        let stranded_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    stranded_document.id,
                    "upload",
                    "sha256:graph-stranded",
                    "Stranded Doc",
                    Some("file:///stranded.txt"),
                ),
            )
            .await
            .context("failed to create stranded revision")?;
        fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: stranded_document.id,
                    active_revision_id: Some(stranded_revision.id),
                    readable_revision_id: Some(stranded_revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote stranded head")?;

        let current_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("graph-current-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create current graph cleanup document")?;
        let current_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    current_document.id,
                    "upload",
                    "sha256:graph-current",
                    "Current Doc",
                    Some("file:///current.txt"),
                ),
            )
            .await
            .context("failed to create current revision")?;
        fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: current_document.id,
                    active_revision_id: Some(current_revision.id),
                    readable_revision_id: Some(current_revision.id),
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote current head")?;

        let pool = &fixture.state.persistence.postgres;
        let projection_version = 1_i64;

        // Seed an entity node supported by both documents. Mirrors what
        // extract_graph would emit for "the same artifact appears in two
        // sources".
        let entity_node = repositories::upsert_runtime_graph_node(
            pool,
            fixture.library_id,
            "entity:shared-artifact",
            "Shared Artifact",
            "artifact",
            None,
            serde_json::json!([]),
            Some("Shared artifact across two documents"),
            serde_json::json!({}),
            2,
            projection_version,
        )
        .await
        .context("failed to seed entity node")?;
        let stranded_doc_node = repositories::upsert_runtime_graph_node(
            pool,
            fixture.library_id,
            &format!("document:{}", stranded_document.id),
            "stranded.txt",
            "document",
            Some(stranded_document.id),
            serde_json::json!([]),
            Some("Stranded document node"),
            serde_json::json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to seed stranded document node")?;
        let current_doc_node = repositories::upsert_runtime_graph_node(
            pool,
            fixture.library_id,
            &format!("document:{}", current_document.id),
            "current.txt",
            "document",
            Some(current_document.id),
            serde_json::json!([]),
            Some("Current document node"),
            serde_json::json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to seed current document node")?;

        let stranded_edge = repositories::upsert_runtime_graph_edge(
            pool,
            fixture.library_id,
            stranded_doc_node.id,
            entity_node.id,
            "mentions",
            &format!("edge:document-{}:entity", stranded_document.id),
            Some("Stranded mention"),
            Some(1.0),
            1,
            serde_json::json!({}),
            projection_version,
        )
        .await
        .context("failed to seed stranded edge")?;
        let current_edge = repositories::upsert_runtime_graph_edge(
            pool,
            fixture.library_id,
            current_doc_node.id,
            entity_node.id,
            "mentions",
            &format!("edge:document-{}:entity", current_document.id),
            Some("Current mention"),
            Some(1.0),
            1,
            serde_json::json!({}),
            projection_version,
        )
        .await
        .context("failed to seed current edge")?;

        let _ = repositories::create_runtime_graph_evidence(
            pool,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind: "node",
                target_id: entity_node.id,
                document_id: Some(stranded_document.id),
                revision_id: Some(stranded_revision.id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("stranded.txt"),
                page_ref: None,
                evidence_text: "stranded mention text",
                confidence_score: Some(0.9),
                evidence_context_key: "stranded:entity",
            },
        )
        .await
        .context("failed to insert stranded entity evidence")?;
        let _ = repositories::create_runtime_graph_evidence(
            pool,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind: "node",
                target_id: entity_node.id,
                document_id: Some(current_document.id),
                revision_id: Some(current_revision.id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("current.txt"),
                page_ref: None,
                evidence_text: "current mention text",
                confidence_score: Some(0.9),
                evidence_context_key: "current:entity",
            },
        )
        .await
        .context("failed to insert current entity evidence")?;
        let _ = repositories::create_runtime_graph_evidence(
            pool,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind: "edge",
                target_id: stranded_edge.id,
                document_id: Some(stranded_document.id),
                revision_id: Some(stranded_revision.id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("stranded.txt"),
                page_ref: None,
                evidence_text: "stranded edge text",
                confidence_score: Some(0.9),
                evidence_context_key: "stranded:edge",
            },
        )
        .await
        .context("failed to insert stranded edge evidence")?;
        let _ = repositories::create_runtime_graph_evidence(
            pool,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind: "edge",
                target_id: current_edge.id,
                document_id: Some(current_document.id),
                revision_id: Some(current_revision.id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("current.txt"),
                page_ref: None,
                evidence_text: "current edge text",
                confidence_score: Some(0.9),
                evidence_context_key: "current:edge",
            },
        )
        .await
        .context("failed to insert current edge evidence")?;

        // Seed canonical summaries for the entity node and one of the edges.
        // No FK cascade exists, so without the targeted cleanup these would
        // outlive their target rows.
        sqlx::query(
            "insert into runtime_graph_canonical_summary (
                workspace_id, library_id, target_kind, target_id,
                summary_text, confidence_status, support_count, source_truth_version
            ) values ($1, $2, 'node', $3, 'entity summary', 'confident', 2, 1)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(entity_node.id)
        .execute(pool)
        .await
        .context("failed to seed entity canonical summary")?;
        sqlx::query(
            "insert into runtime_graph_canonical_summary (
                workspace_id, library_id, target_kind, target_id,
                summary_text, confidence_status, support_count, source_truth_version
            ) values ($1, $2, 'edge', $3, 'stranded edge summary', 'confident', 1, 1)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(stranded_edge.id)
        .execute(pool)
        .await
        .context("failed to seed stranded edge canonical summary")?;

        // Mark the stranded document as already soft-deleted, simulating a
        // failed prior cleanup. This leaves its evidence rows in place — the
        // canonical fix must sweep them on the next delete in this library.
        sqlx::query(
            "update content_document
             set document_state = 'deleted', deleted_at = now()
             where id = $1",
        )
        .bind(stranded_document.id)
        .execute(pool)
        .await
        .context("failed to mark stranded document deleted")?;

        // Sanity: orphan rows currently survive against the deleted doc.
        let stranded_evidence_before: i64 =
            sqlx::query_scalar("select count(*) from runtime_graph_evidence where document_id = $1")
                .bind(stranded_document.id)
                .fetch_one(pool)
                .await
                .context("failed to count stranded evidence pre-delete")?;
        assert_eq!(
            stranded_evidence_before, 2,
            "test setup must leave stranded evidence rows for the canonical fix to sweep"
        );

        // Delete the current document via the canonical service path.
        let delete_admission = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: current_document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some("graph-cleanup-current-doc".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .context("delete must succeed")?;
        assert_eq!(delete_admission.mutation.mutation_state, "applied");

        // Evidence for both docs is gone (current via the explicit branch,
        // stranded via the library-wide orphan sweep).
        let evidence_after: i64 = sqlx::query_scalar(
            "select count(*) from runtime_graph_evidence where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(pool)
        .await
        .context("failed to count evidence post-delete")?;
        assert_eq!(
            evidence_after, 0,
            "library-wide sweep must remove evidence for every soft-deleted document, \
             including ones whose previous cleanup failed"
        );

        // Entity node and both document-typed nodes are gone.
        let surviving_node_ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from runtime_graph_node where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_all(pool)
        .await
        .context("failed to list surviving graph nodes")?;
        assert!(
            !surviving_node_ids.contains(&entity_node.id),
            "entity node with zero active-document support must be pruned"
        );
        assert!(
            !surviving_node_ids.contains(&stranded_doc_node.id),
            "document-typed node for stranded doc must be pruned"
        );
        assert!(
            !surviving_node_ids.contains(&current_doc_node.id),
            "document-typed node for current doc must be pruned"
        );

        let surviving_edge_count: i64 =
            sqlx::query_scalar("select count(*) from runtime_graph_edge where library_id = $1")
                .bind(fixture.library_id)
                .fetch_one(pool)
                .await
                .context("failed to count surviving edges")?;
        assert_eq!(
            surviving_edge_count, 0,
            "every edge whose endpoints both lost support must be pruned"
        );

        // Canonical summary rows for pruned targets are gone too.
        let summary_count: i64 = sqlx::query_scalar(
            "select count(*) from runtime_graph_canonical_summary where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(pool)
        .await
        .context("failed to count surviving canonical summaries")?;
        assert_eq!(
            summary_count, 0,
            "canonical summaries for pruned nodes/edges must be removed since the table has no FK cascade"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
