use crate::{content_lifecycle_support, web_ingest_support};

#[cfg(feature = "test-support")]
use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use ironrag_backend::{
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        knowledge_plane::DocumentPointersUpdate,
        knowledge_rows::KnowledgeStructuredRevisionRow,
        repositories::{self, iam_repository},
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::service::{
            AdmitMutationCommand, AppendInlineMutationCommand, CreateDocumentCommand,
            PromoteHeadCommand, ReplaceInlineMutationCommand, RevisionAdmission,
            UpdateMutationCommand, UploadInlineDocumentCommand,
        },
        knowledge::service::PromoteKnowledgeDocumentCommand,
    },
};

#[cfg(feature = "test-support")]
use ironrag_backend::services::ingest::web::CreateWebIngestRunCommand;

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

async fn irg57_seed_document(fixture: &ContentLifecycleFixture) -> Result<Uuid> {
    let content = &fixture.state.application_services.content;
    let document = content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some(format!("irg57-{}", Uuid::now_v7())),
                file_name: Some("irg57.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await?;
    let revision = content_lifecycle_support::create_imported_revision(
        &fixture.state,
        revision_command(document.id, "upload", "sha256:irg57", "IRG-57", None),
        "irg57.txt",
    )
    .await?;
    seed_prepared_revision_marker(fixture, document.id, revision.id, 0).await?;
    let owner = content_lifecycle_support::publication_item(
        &fixture.state.persistence.postgres,
        revision.id,
        "irg57.txt",
    )
    .await?;
    content
        .promote_document_head(
            &fixture.state,
            PromoteHeadCommand {
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                publishing_mutation_item_id: owner,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;
    Ok(document.id)
}

// Compare persisted source, heads, revisions and readiness, not just lifecycle flags.
async fn irg57_source_snapshot(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar(
        "select jsonb_build_object(
            'document', (select to_jsonb(d) from content_document d where id = $1),
            'head', (select to_jsonb(h) from content_document_head h where document_id = $1),
            'revisions', (select jsonb_agg(to_jsonb(r) order by id) from content_revision r where document_id = $1),
            'projection_revisions', (select jsonb_agg(to_jsonb(r) order by revision_id) from knowledge_revision r where document_id = $1),
            'readiness', (select jsonb_agg(to_jsonb(r) order by revision_id) from knowledge_structured_revision r where document_id = $1))",
    )
    .bind(document_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?)
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn irg57_projection_repair_restores_source_lifecycle_idempotently() -> Result<()> {
    use ironrag_backend::services::maintenance::knowledge_projection_metadata::knowledge_projection_metadata;

    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        let document_id = irg57_seed_document(&fixture).await?;
        let healthy_id = irg57_seed_document(&fixture).await?;
        let deleted_id = irg57_seed_document(&fixture).await?;
        fixture.state.application_services.content.delete_document(&fixture.state, deleted_id).await?;
        // A deleted source is ineligible even if its projection claims to be active.
        sqlx::query("update knowledge_document set document_state = 'active', deleted_at = null where document_id = $1")
            .bind(deleted_id).execute(pool).await?;
        // Exercise each field alone as well as a complete knowledge tombstone.
        let timestamp_only_id = irg57_seed_document(&fixture).await?;
        let state_only_id = irg57_seed_document(&fixture).await?;
        let wrong_scope_id = irg57_seed_document(&fixture).await?;
        let other_library = fixture.state.application_services.catalog.create_library(
            &fixture.state,
            ironrag_backend::services::catalog_service::CreateLibraryCommand {
                workspace_id: fixture.workspace_id,
                slug: Some(format!("irg57-other-{}", Uuid::now_v7().simple())),
                display_name: "IRG-57 scope control".to_string(),
                description: None,
                created_by_principal_id: None,
            },
        ).await?;
        sqlx::query("update knowledge_document set library_id = $2, document_state = 'deleted' where document_id = $1")
            .bind(wrong_scope_id).bind(other_library.id).execute(pool).await?;
        sqlx::query("update knowledge_document set document_state = 'deleted', deleted_at = now() where document_id = $1")
            .bind(document_id).execute(pool).await?;
        sqlx::query("update knowledge_document set deleted_at = now() where document_id = $1")
            .bind(timestamp_only_id).execute(pool).await?;
        sqlx::query("update knowledge_document set document_state = 'deleted' where document_id = $1")
            .bind(state_only_id).execute(pool).await?;
        let source_before = irg57_source_snapshot(&fixture, document_id).await?;
        let timestamp_source_before = irg57_source_snapshot(&fixture, timestamp_only_id).await?;
        let controls_before: serde_json::Value = sqlx::query_scalar("select jsonb_agg(to_jsonb(k) order by document_id) from knowledge_document k where document_id = any($1)")
            .bind(vec![healthy_id, deleted_id, wrong_scope_id]).fetch_one(pool).await?;
        let generation_before = repositories::get_library_source_truth_version(pool, fixture.library_id).await?;
        let dry = knowledge_projection_metadata(&fixture.state, Some(fixture.library_id), true).await?;
        assert_eq!(dry.document_rows_updated, 3, "dry-run must count lifecycle-only drift");
        assert_eq!(dry.revision_rows_updated, 0);
        assert_eq!(dry.libraries_reconciled, 1);
        let still_drifted: i64 = sqlx::query_scalar("select count(*) from knowledge_document where document_id = any($1) and deleted_at is not null")
            .bind(vec![document_id, timestamp_only_id]).fetch_one(pool).await?;
        assert_eq!(still_drifted, 2, "dry-run must not write");
        assert_eq!(repositories::get_library_source_truth_version(pool, fixture.library_id).await?, generation_before);
        let applied = knowledge_projection_metadata(&fixture.state, Some(fixture.library_id), false).await?;
        assert_eq!(applied.document_rows_updated, 3);
        assert_eq!(applied.revision_rows_updated, 0);
        let restored: i64 = sqlx::query_scalar("select count(*) from knowledge_document where document_id = any($1) and document_state = 'active' and deleted_at is null")
            .bind(vec![document_id, timestamp_only_id, state_only_id]).fetch_one(pool).await?;
        assert_eq!(restored, 3);
        assert_eq!(irg57_source_snapshot(&fixture, document_id).await?, source_before);
        assert_eq!(irg57_source_snapshot(&fixture, timestamp_only_id).await?, timestamp_source_before);
        let controls_after: serde_json::Value = sqlx::query_scalar("select jsonb_agg(to_jsonb(k) order by document_id) from knowledge_document k where document_id = any($1)")
            .bind(vec![healthy_id, deleted_id, wrong_scope_id]).fetch_one(pool).await?;
        assert_eq!(controls_after, controls_before);
        for dry_run in [true, false] {
            let wrong_scope = knowledge_projection_metadata(&fixture.state, Some(other_library.id), dry_run).await?;
            assert_eq!(wrong_scope.document_rows_updated, 0, "cross-library source must not be copied");
            assert_eq!(wrong_scope.revision_rows_updated, 0);
        }
        let generation_after = repositories::get_library_source_truth_version(pool, fixture.library_id).await?;
        assert!(generation_after > generation_before);
        for dry_run in [true, false] {
            let rerun = knowledge_projection_metadata(&fixture.state, Some(fixture.library_id), dry_run).await?;
            assert_eq!(rerun.document_rows_updated, 0);
            assert_eq!(rerun.revision_rows_updated, 0);
            assert_eq!(rerun.libraries_reconciled, 0);
        }
        assert_eq!(repositories::get_library_source_truth_version(pool, fixture.library_id).await?, generation_after);
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn irg57_materializer_copies_authoritative_lifecycle() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document_id = irg57_seed_document(&fixture).await?;
        let pool = &fixture.state.persistence.postgres;
        sqlx::query("update knowledge_document set document_state = 'deleted', deleted_at = now() where document_id = $1")
            .bind(document_id).execute(pool).await?;
        let before = irg57_source_snapshot(&fixture, document_id).await?;
        repositories::content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(pool, document_id).await?;
        let lifecycle: (String, Option<chrono::DateTime<Utc>>) = sqlx::query_as("select document_state, deleted_at from knowledge_document where document_id = $1")
            .bind(document_id).fetch_one(pool).await?;
        assert_eq!(lifecycle, ("active".to_string(), None));
        assert_eq!(irg57_source_snapshot(&fixture, document_id).await?, before);
        fixture.state.application_services.content.delete_document(&fixture.state, document_id).await?;
        let matches: bool = sqlx::query_scalar("select k.document_state = cd.document_state::text and k.deleted_at is not distinct from cd.deleted_at and k.deleted_at is not null from knowledge_document k join content_document cd on cd.id = k.document_id where cd.id = $1")
            .bind(document_id).fetch_one(pool).await?;
        assert!(matches, "existing projection must also copy the authoritative tombstone");
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn irg57_delete_projection_failure_rolls_back_lifecycle() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document_id = irg57_seed_document(&fixture).await?;
        let pool = &fixture.state.persistence.postgres;
        let before = irg57_source_snapshot(&fixture, document_id).await?;
        let projection_before: serde_json::Value = sqlx::query_scalar("select to_jsonb(k) from knowledge_document k where document_id = $1")
            .bind(document_id).fetch_one(pool).await?;
        let generation_before = repositories::get_library_source_truth_version(pool, fixture.library_id).await?;
        // This constraint exists only in this fixture's private temporary database.
        let outbox_before: serde_json::Value = sqlx::query_scalar("select coalesce(jsonb_agg(to_jsonb(o) order by id), '[]'::jsonb) from webhook_lifecycle_outbox o")
            .fetch_one(pool).await?;
        sqlx::query("alter table knowledge_document add constraint irg57_reject_tombstone check (document_state <> 'deleted')")
            .execute(pool).await?;
        fixture.state.application_services.content.delete_document(&fixture.state, document_id).await.expect_err("projection failure must abort deletion");
        assert_eq!(irg57_source_snapshot(&fixture, document_id).await?, before);
        let projection_after: serde_json::Value = sqlx::query_scalar("select to_jsonb(k) from knowledge_document k where document_id = $1")
            .bind(document_id).fetch_one(pool).await?;
        assert_eq!(projection_after, projection_before);
        let outbox_after: serde_json::Value = sqlx::query_scalar("select coalesce(jsonb_agg(to_jsonb(o) order by id), '[]'::jsonb) from webhook_lifecycle_outbox o")
            .fetch_one(pool).await?;
        assert_eq!(outbox_after, outbox_before, "failed deletion must not enqueue its webhook");
        assert_eq!(repositories::get_library_source_truth_version(pool, fixture.library_id).await?, generation_before);
        sqlx::query("alter table knowledge_document drop constraint irg57_reject_tombstone").execute(pool).await?;
        fixture.state.application_services.content.delete_document(&fixture.state, document_id).await?;
        let matches: bool = sqlx::query_scalar("select cd.document_state = 'deleted' and k.document_state = cd.document_state::text and k.deleted_at is not distinct from cd.deleted_at and k.deleted_at is not null from knowledge_document k join content_document cd on cd.id = k.document_id where cd.id = $1")
            .bind(document_id).fetch_one(pool).await?;
        assert!(matches);
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

async fn seed_prepared_revision_marker(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_count: i32,
) -> Result<()> {
    let now = Utc::now();
    fixture
        .state
        .document_store
        .replace_structured_revision_artifact(
            &KnowledgeStructuredRevisionRow {
                revision_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                preparation_state: "prepared".to_string(),
                normalization_profile: "content-lifecycle-test-v1".to_string(),
                source_format: "text".to_string(),
                language_code: None,
                block_count: 0,
                chunk_count,
                typed_fact_count: 0,
                outline_json: serde_json::json!([]),
                prepared_at: now,
                updated_at: now,
            },
            &[],
        )
        .await
        .context("failed to seed prepared revision marker")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_document_creation_rolls_back_when_projection_insert_fails() -> Result<()> {
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
            .application_services
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
            .expect_err(
                "a failed projection insert must fail the authoritative create transaction",
            );

        let authoritative_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from content_document
             where library_id = $1 and external_key = $2",
        )
        .bind(fixture.library_id)
        .bind(&external_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(
            authoritative_count, 0,
            "projection failure must roll back the authoritative document and head",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_revision_creation_self_heals_or_rolls_back_as_one_transaction() -> Result<()>
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

        let authoritative_count = sqlx::query_scalar::<_, i64>(
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
        assert_eq!(authoritative_count, 1, "failed second revision must roll back consistently");
        assert_eq!(projection_count, authoritative_count, "revision planes must retain parity");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn materializer_inserts_missing_projection_and_uses_only_authoritative_metadata() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
        let external_key = format!("authoritative-metadata-{}", Uuid::now_v7());
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(external_key.clone()),
                    file_name: Some("authoritative-input.txt".to_string()),
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
            Some("authoritative-input.txt"),
            "the admitted safe file name must be persisted on the authoritative-backed projection",
        );

        sqlx::query("delete from knowledge_document where document_id = $1")
            .bind(document.id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to remove document projection")?;
        let outcome =
            repositories::content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
                &fixture.state.persistence.postgres,
                document.id,
            )
            .await
            .context("materializer failed to recreate a missing projection")?;
        assert_eq!(
            outcome,
            repositories::content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized,
        );
        let recreated = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("materializer did not recreate the document projection")?;
        assert_eq!(recreated.external_key, external_key);
        assert_eq!(recreated.file_name.as_deref(), Some("authoritative-input.txt"));
        assert_eq!(recreated.title, None);

        let mut command = revision_command(
            document.id,
            "upload",
            "sha256:authoritative-metadata",
            "temporary title",
            None,
        );
        command.title = None;
        command.storage_key = Some(format!(
            "content/{}/{}/{}-authoritative-report.pdf",
            fixture.workspace_id,
            fixture.library_id,
            "a".repeat(64),
        ));
        let revision = content_lifecycle_support::create_imported_revision(&fixture.state, command, "authoritative-report.pdf")
            .await
            .context("failed to create authoritative metadata revision")?;
        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    publishing_mutation_item_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to establish authoritative metadata head")?;
        sqlx::query(
            "update knowledge_document
             set title = 'stale title', file_name = 'stale-name.bin'
             where document_id = $1",
        )
        .bind(document.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        repositories::content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
            &fixture.state.persistence.postgres,
            document.id,
        )
        .await
        .context("failed to rematerialize authoritative metadata")?;
        let rematerialized = fixture
            .state
            .document_store
            .get_document(document.id)
            .await?
            .context("rematerialized document projection missing")?;
        assert_eq!(
            rematerialized.title, None,
            "a authoritative NULL title must clear stale projected title",
        );
        assert_eq!(
            rematerialized.file_name.as_deref(),
            Some("authoritative-report.pdf"),
            "file name must be deterministically reconstructed from authoritative storage metadata",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn failed_head_projection_rolls_back_the_authoritative_head() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
        let revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:head-rollback",
                "Blocked projection title",
                Some("upload://head-rollback.txt"),
            ),
            "head-rollback.txt",
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
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        revision.id,
                        "head-rollback.txt",
                    )
                    .await?,
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

        let head = repositories::content_document_head_repository::get_document_head(
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
#[ignore = "requires local postgres with required extensions"]
async fn chunk_projection_failure_rolls_back_both_chunk_planes() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
        let revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:chunk-atomic",
                "Chunk atomic",
                Some("upload://chunk-atomic.txt"),
            ),
            "chunk-atomic.txt",
        )
        .await
        .context("failed to create chunk atomic revision")?;
        let original = repositories::content_chunk_repository::NewContentChunkProjection {
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
        repositories::content_chunk_repository::replace_chunks_with_projection(
            &fixture.state.persistence.postgres,
            revision.id,
            std::slice::from_ref(&original),
        )
        .await
        .context("failed to establish atomic chunk baseline")?;
        seed_prepared_revision_marker(&fixture, document.id, revision.id, 1).await?;
        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        revision.id,
                        "chunk-atomic.txt",
                    )
                    .await?,
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
        let replacement_result =
            repositories::content_chunk_repository::replace_chunks_with_projection(
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

        let authoritative = repositories::content_chunk_repository::list_chunks_by_revision(
            &fixture.state.persistence.postgres,
            revision.id,
        )
        .await?;
        let projected = fixture.state.document_store.list_chunks_by_revision(revision.id).await?;
        assert_eq!(authoritative.len(), 1);
        assert_eq!(projected.len(), 1);
        assert_eq!(authoritative[0].normalized_text, "original evidence");
        assert_eq!(projected[0].content_text, "original evidence");
        assert_eq!(authoritative[0].id, projected[0].chunk_id);
        let fingerprint =
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            !fingerprint.is_catching_up,
            "rolled-back replacement must retain authoritative/document/revision/chunk parity",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn image_attachment_promotion_keeps_authoritative_and_knowledge_roles_in_sync() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
                    // Keep the authoritative external key intentionally opaque:
                    // media classification must use the persisted file name.
                    external_key: Some(format!("role-image-{}", Uuid::now_v7())),
                    file_name: Some("diagram.png".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: Some(parent_key),
                },
            )
            .await
            .context("failed to create role-sync image attachment")?;
        let authoritative_before =
            repositories::content_document_repository::get_document_by_id(pool, image.id)
                .await?
                .context("authoritative image attachment missing before promotion")?;
        assert_eq!(authoritative_before.parent_document_id, Some(parent.id));
        assert_eq!(authoritative_before.document_role, "attachment");

        let mut image_revision_command = revision_command(
            image.id,
            "upload",
            "sha256:role-image",
            "Role image",
            Some("upload://role-image"),
        );
        image_revision_command.mime_type = "image/png".to_string();
        let revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            image_revision_command,
            "diagram.png",
        )
        .await
        .context("failed to create role-sync image revision")?;

        content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: image.id,
                    active_revision_id: Some(revision.id),
                    readable_revision_id: Some(revision.id),
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        revision.id,
                        "diagram.png",
                    )
                    .await?,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote role-sync image attachment")?;

        let authoritative =
            repositories::content_document_repository::get_document_by_id(pool, image.id)
                .await?
                .context("authoritative image attachment missing after promotion")?;
        let knowledge = fixture
            .state
            .document_store
            .get_document(image.id)
            .await?
            .context("knowledge image attachment missing after promotion")?;
        assert_eq!(authoritative.document_role, "attached_context");
        assert_eq!(knowledge.document_role, authoritative.document_role);

        let fingerprint =
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                pool,
                fixture.library_id,
            )
            .await?;
        assert!(
            !fingerprint.is_catching_up,
            "a successful promotion must not leave document-role projection drift",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn delayed_older_promotion_cannot_overwrite_superseding_authoritative_head() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
        let older_revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:promotion-race-older",
                "Older revision",
                Some("upload://promotion-race-older"),
            ),
            "promotion-race.txt",
        )
        .await
        .context("failed to create older promotion-race revision")?;
        let newer_revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "append",
                "sha256:promotion-race-newer",
                "Newer revision",
                Some("upload://promotion-race-newer"),
            ),
            "promotion-race.txt",
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
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        older_revision.id,
                        "promotion-race.txt",
                    )
                    .await?,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to establish older promotion-race head")?;

        let document_id = document.id;
        let newer_revision_id = newer_revision.id;

        // Hold B's parent-first lifecycle lock while the delayed projection
        // phase of A starts. B then advances the authoritative head inside that
        // transaction. A must serialize on the parent, observe B after commit,
        // and materialize B rather than its stale command payload.
        let newer_item_id = content_lifecycle_support::publication_item(
            &fixture.state.persistence.postgres,
            newer_revision_id,
            "promotion-race.txt",
        )
        .await?;
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
                .application_services
                .knowledge
                .promote_document(&delayed_state, PromoteKnowledgeDocumentCommand { document_id })
                .await
                .map_err(|error| anyhow::anyhow!("delayed older promotion failed: {error}"))
        });
        start_barrier.wait().await;
        tokio::task::yield_now().await;
        repositories::content_document_head_repository::upsert_document_head_with_executor(
            &mut *superseding_transaction,
            &repositories::content_document_head_repository::NewContentDocumentHead {
                document_id,
                active_revision_id: Some(newer_revision_id),
                readable_revision_id: Some(newer_revision_id),
                publishing_mutation_item_id: newer_item_id,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to write superseding authoritative promotion-race head")?;
        superseding_transaction
            .commit()
            .await
            .context("failed to commit superseding promotion-race transaction")?;
        tokio::time::timeout(std::time::Duration::from_secs(10), delayed_older_promotion)
            .await
            .context("delayed promotion did not resume after superseding commit")?
            .context("delayed promotion task panicked")??;

        let authoritative_head = repositories::content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?
        .context("authoritative promotion-race head missing")?;
        let knowledge = fixture
            .state
            .document_store
            .get_document(document_id)
            .await?
            .context("knowledge promotion-race document missing")?;
        assert_eq!(authoritative_head.active_revision_id, Some(newer_revision_id));
        assert_eq!(authoritative_head.readable_revision_id, Some(newer_revision_id));
        assert_eq!(knowledge.active_revision_id, authoritative_head.active_revision_id);
        assert_eq!(knowledge.readable_revision_id, authoritative_head.readable_revision_id);
        assert_eq!(knowledge.latest_revision_no, Some(2));
        assert_eq!(knowledge.title.as_deref(), Some("Newer revision"));

        let fingerprint =
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            !fingerprint.is_catching_up,
            "an out-of-order stale promotion must converge to the superseding authoritative head",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn delete_parent_cascades_attached_context_and_detaches_attachment_children() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
            let row = repositories::content_document_repository::get_document_by_id(pool, child_id)
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
        let image_row =
            repositories::content_document_repository::get_document_by_id(pool, image_child.id)
                .await?
                .context("image child row missing after cascade")?;
        assert_eq!(
            image_row.document_state, "deleted",
            "attached_context child must be cascade-soft-deleted with its parent"
        );

        // attachment child survives, detached but keeps its declared key.
        let attachment_row = repositories::content_document_repository::get_document_by_id(
            pool,
            attachment_child.id,
        )
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
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_promotes_head_and_separates_readable_from_active()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let document = fixture
            .state
            .application_services
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
        let readable_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:head-readable",
                    "Readable Revision",
                    Some("file:///readable.txt"),
                ),
                "readable.txt",
            )
            .await
            .context("failed to create readable revision")?;
        // Explicit append bases must already be public. Extraction alone does
        // not supply published ownership, and an unpromoted revision is not a base.
        let readable_item_id = content_lifecycle_support::publication_item(
            &fixture.state.persistence.postgres, readable_revision.id, "readable.txt",
        ).await?;
        use sha2::{Digest, Sha256};
        let base_text_checksum = format!("sha256:{}",hex::encode(Sha256::digest(b"Readable base evidence")));
        fixture.state.application_services.knowledge.set_revision_extract_state(
            &fixture.state, readable_revision.id, "ready", Some("Readable base evidence"),
            Some(&base_text_checksum),
        ).await.context("failed to extract the explicit append base")?;
        let source_truth_before_base = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres, fixture.library_id,
        ).await?;
        let base_head = fixture.state.application_services.content.promote_document_head(
            &fixture.state, PromoteHeadCommand {
                document_id:document.id,active_revision_id:Some(readable_revision.id),
                readable_revision_id:Some(readable_revision.id),publishing_mutation_item_id:readable_item_id,
                latest_mutation_id:None,latest_successful_attempt_id:None,
            },
        ).await.context("failed to publish the explicit append base")?;
        assert_eq!(base_head.visibility_intent_id,None);
        assert_eq!(base_head.readable_revision_id,Some(readable_revision.id));
        let current_base: Uuid = sqlx::query_scalar(
            "select revision_id from content_current_revision where document_id=$1",
        ).bind(document.id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(current_base,readable_revision.id,"append must consume a published exact base");
        assert!(repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,fixture.library_id,
        ).await? > source_truth_before_base,"publishing the readable base must invalidate answer cache identities");
        use repositories::admission_repository::{ContentAdmissionRequest,ContentAdmissionTarget,RevisionAdmission,admit_content_with_failpoint};
        let admission = admit_content_with_failpoint(
            &fixture.state.persistence.postgres,
            &ContentAdmissionRequest {
                workspace_id:fixture.workspace_id,library_id:fixture.library_id,
                operation_kind:"append".to_owned(),requested_by_principal_id:None,
                request_surface:"rest".to_owned(),idempotency_key:None,source_identity:None,
                target:ContentAdmissionTarget::Existing {document_id:document.id},
                revision:Some(RevisionAdmission {
                    content_source_kind:"append".to_owned(),base_revision_id:Some(readable_revision.id),visibility_intent_id:None,
                    replacement:ironrag_contracts::documents::ReplacementIdentity {
                        checksum:format!("sha256:{}","a".repeat(64)),mime_type:"text/plain".to_owned(),byte_size:128,
                        file_name:"active.txt".to_owned(),title:Some("Active Revision".to_owned()),
                        language_code:Some("en".to_owned()),source_uri:None,document_hint:None,
                    },storage_key:Some("memory://active.txt".to_owned()),
                }),parent_async_operation_id:None,priority:10,
            },None,
        ).await.context("failed to admit canonical append mutation")?.into_bundle();
        let active_revision=admission.revision.context("append revision missing")?;
        let mutation=admission.mutation;
        let item=admission.item.context("append item missing")?;
        assert_eq!(item.base_revision_id,Some(readable_revision.id));
        assert_eq!(item.result_revision_id,Some(active_revision.id));
        assert_eq!(active_revision.parent_revision_id,Some(readable_revision.id));
        assert_eq!(item.item_state,"pending");
        assert!(admission.job.is_some(),"append admission must own its processing job");
        assert_eq!(admission.async_operation.status,"accepted");

        let source_truth_before = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;

        let promoted_head = fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(active_revision.id),
                    readable_revision_id: Some(readable_revision.id),
                    publishing_mutation_item_id: None,
                    latest_mutation_id: Some(mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote document head")?;
        assert_eq!(promoted_head.active_revision_id, Some(active_revision.id));
        assert_eq!(promoted_head.readable_revision_id, Some(readable_revision.id));
        assert_eq!(promoted_head.readable_revision_id,Some(readable_revision.id));
        assert_eq!(promoted_head.visibility_intent_id,None);
        assert_eq!(promoted_head.latest_mutation_id, Some(mutation.id));
        let source_truth_after = repositories::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        assert!(
            source_truth_after > source_truth_before,
            "a committed active/readable projection change must invalidate cross-replica answer cache identities"
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

        let empty_catching_up_identity =
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            empty_catching_up_identity.is_catching_up,
            "an unfinished active revision must mark an empty library as catching up",
        );
        assert!(
            !empty_catching_up_identity.has_readable_content,
            "a library with no ready chunk on any readable revision must remain not ready",
        );

        let fully_readable_document = fixture
            .state
            .application_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("fully-readable-doc-{}", Uuid::now_v7())),
                    file_name: Some("fully-readable.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await
            .context("failed to create fully readable document")?;
        let fully_readable_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    fully_readable_document.id,
                    "upload",
                    "sha256:fully-readable",
                    "Fully Readable Revision",
                    Some("file:///fully-readable.txt"),
                ),
                "fully-readable.txt",
            )
            .await
            .context("failed to create fully readable revision")?;
        fixture
            .state
            .application_services
            .knowledge
            .set_revision_extract_state(
                &fixture.state,
                fully_readable_revision.id,
                "ready",
                Some("Stable readable evidence"),
                Some("sha256:9235a67959d77ca6bdc8a80abfb859b6577ddb4ee412e0656e57bb78cfd5c840"),
            )
            .await
            .context("failed to mark the fully readable revision extracted")?;
        repositories::content_chunk_repository::replace_chunks_with_projection(
            &fixture.state.persistence.postgres,
            fully_readable_revision.id,
            &[repositories::content_chunk_repository::NewContentChunkProjection {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: fully_readable_document.id,
                revision_id: fully_readable_revision.id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 24,
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                content_text: "Stable readable evidence".to_string(),
                normalized_text: "Stable readable evidence".to_string(),
                text_checksum:
                    "sha256:9235a67959d77ca6bdc8a80abfb859b6577ddb4ee412e0656e57bb78cfd5c840"
                        .to_string(),
                support_block_ids: Vec::new(),
                section_path: vec!["Overview".to_string()],
                heading_trail: vec!["Overview".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(i64::from(fully_readable_revision.revision_number)),
                vector_generation: None,
                quality_score: Some(1.0),
                window_text: Some("Stable readable evidence".to_string()),
                occurred_at: None,
                occurred_until: None,
            }],
        )
        .await
        .context("failed to project the fully readable chunk")?;
        seed_prepared_revision_marker(
            &fixture,
            fully_readable_document.id,
            fully_readable_revision.id,
            1,
        )
        .await?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: fully_readable_document.id,
                    active_revision_id: Some(fully_readable_revision.id),
                    readable_revision_id: Some(fully_readable_revision.id),
                    publishing_mutation_item_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote the fully readable document")?;

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
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            divergent_identity.is_catching_up,
            "one lagging document must disable cached answer replay"
        );
        assert!(
            divergent_identity.has_readable_content,
            "another fully readable document must keep fresh answer generation available"
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
            repositories::content_fingerprint_repository::get_library_readable_content_fingerprint(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await?;
        assert!(
            converged_identity.is_catching_up,
            "an unfinished active revision must remain catching up without a running ingest job",
        );

        fixture
            .state
            .application_services
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

        let authoritative_before_missing_projection =
            repositories::content_revision_repository::get_revision_by_id(
                &fixture.state.persistence.postgres,
                readable_revision.id,
            )
            .await?
            .context("authoritative readable revision missing before rollback regression")?;
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
            .application_services
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
        let authoritative_after_missing_projection =
            repositories::content_revision_repository::get_revision_by_id(
                &fixture.state.persistence.postgres,
                readable_revision.id,
            )
            .await?
            .context("authoritative readable revision disappeared after rollback regression")?;
        assert_eq!(
            authoritative_after_missing_projection.document_hint,
            authoritative_before_missing_projection.document_hint,
            "the authoritative hint must roll back with the missing projection",
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
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_edit_mutation_persists_source_for_reprocess() -> Result<()>
{
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
            .application_services
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
        let base_revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:edit-base",
                "Inventory.xlsx",
                Some("upload://inventory.xlsx"),
            ),
            "inventory.xlsx",
        )
        .await
        .context("failed to create edit base revision")?;
        fixture
            .state
            .application_services
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
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(base_revision.id),
                    readable_revision_id: Some(base_revision.id),
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        base_revision.id,
                        "inventory.xlsx",
                    )
                    .await?,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote edit base head")?;

        let edit_command = || ReplaceInlineMutationCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: document.id,
            idempotency_key: Some("authoritative-edit".to_string()),
            requested_by_principal_id: Some(principal.id),
            request_surface: "rest".to_string(),
            source_identity: None,
            visibility_intent_id: None,
            file_bytes: edited_markdown.as_bytes().to_vec(),
            file_name: "Edited-note.md".to_owned(),
            mime_type: Some("text/markdown".to_owned()),
            title: None,
            language_code: None,
            source_uri: None,
            document_hint: None,
        };
        let edit_admission = fixture
            .state
            .application_services
            .content
            .replace_inline_mutation(&fixture.state, edit_command())
            .await
            .context("failed to admit authoritative edit mutation")?;

        assert_eq!(edit_admission.mutation.mutation_state, "accepted");
        let edit_revision_id = edit_admission
            .items
            .first()
            .and_then(|item| item.result_revision_id)
            .context("edit admission must create a revision")?;
        let edit_revision = fixture
            .state
            .application_services
            .content
            .list_revisions(&fixture.state, document.id)
            .await
            .context("failed to list revisions after edit admission")?
            .into_iter()
            .find(|revision| revision.id == edit_revision_id)
            .context("edited revision missing from revision list")?;
        assert_eq!(edit_revision.content_source_kind, "replace");
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
            .application_services
            .content
            .resolve_revision_storage_key(&fixture.state, edit_revision.id)
            .await
            .context("failed to resolve stored key for edited revision")?;
        assert_eq!(resolved_storage_key.as_deref(), Some(storage_key.as_str()));

        let repeated_edit_admission = fixture
            .state
            .application_services
            .content
            .replace_inline_mutation(&fixture.state, edit_command())
            .await
            .context("failed to replay authoritative edit mutation")?;
        assert_eq!(repeated_edit_admission.mutation.id, edit_admission.mutation.id);
        assert_eq!(repeated_edit_admission.job_id, edit_admission.job_id);
        assert_eq!(
            repeated_edit_admission.items.first().and_then(|item| item.result_revision_id),
            Some(edit_revision.id)
        );
        fixture
            .state
            .application_services
            .content
            .update_mutation(
                &fixture.state,
                UpdateMutationCommand {
                    mutation_id: edit_admission.mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await
            .context("failed to complete authoritative edit mutation")?;

        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(edit_revision.id),
                    readable_revision_id: Some(base_revision.id),
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        base_revision.id,
                        "inventory.xlsx",
                    )
                    .await?,
                    latest_mutation_id: Some(edit_admission.mutation.id),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote edited revision as active head")?;

        let reprocess_admission = fixture
            .state
            .application_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "reprocess".to_string(),
                    idempotency_key: Some("authoritative-edit-reprocess".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: Some(RevisionAdmission {
                        content_source_kind: edit_revision.content_source_kind.clone(),
                        replacement: ironrag_contracts::documents::ReplacementIdentity {
                            file_name: "edited.md".to_owned(),
                            checksum: edit_revision.checksum.clone(),
                            mime_type: edit_revision.mime_type.clone(),
                            byte_size: edit_revision.byte_size,
                            title: edit_revision.title.clone(),
                            language_code: edit_revision.language_code.clone(),
                            source_uri: edit_revision.source_uri.clone(),
                            document_hint: edit_revision.document_hint.clone(),
                        },
                        visibility_intent_id: None,
                        base_revision_id: None,
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
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_concurrent_identical_uploads_admit_one_document()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
        let file_bytes = b"Concurrent duplicate upload fixture.".to_vec();
        let first_command = UploadInlineDocumentCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            external_key: Some(format!("concurrent-upload-a-{}", Uuid::now_v7())),
            idempotency_key: None,
            requested_by_principal_id: None,
            request_surface: "rest".to_string(),
            source_identity: Some("concurrent-upload-a".to_string()),
            file_name: "concurrent-a.txt".to_string(),
            title: Some("Concurrent upload A".to_string()),
            document_hint: None,
            mime_type: Some("text/plain".to_string()),
            file_bytes: file_bytes.clone(),
            parent_external_key: None,
        };
        let second_command = UploadInlineDocumentCommand {
            external_key: Some(format!("concurrent-upload-b-{}", Uuid::now_v7())),
            source_identity: Some("concurrent-upload-b".to_string()),
            file_name: "concurrent-b.txt".to_string(),
            title: Some("Concurrent upload B".to_string()),
            file_bytes,
            ..first_command.clone()
        };

        // Hold the same advisory lock before either upload starts. Both tasks
        // must block before their checksum lookup; with the old pre-lock lookup
        // they would both observe absence and both admit after this lock drops.
        let blocker =
            repositories::content_mutation_repository::acquire_content_library_storage_lock(
                &fixture.state.persistence.postgres,
                fixture.library_id,
            )
            .await
            .context("failed to acquire duplicate-upload test blocker")?;
        let first_state = fixture.state.clone();
        let second_state = fixture.state.clone();
        let first_upload = tokio::spawn(async move {
            first_state
                .application_services
                .content
                .upload_inline_document(&first_state, first_command)
                .await
        });
        let second_upload = tokio::spawn(async move {
            second_state
                .application_services
                .content
                .upload_inline_document(&second_state, second_command)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!first_upload.is_finished(), "first upload bypassed the library lock");
        assert!(!second_upload.is_finished(), "second upload bypassed the library lock");
        blocker.commit().await.context("failed to release duplicate-upload test blocker")?;

        let first_result = first_upload.await.context("first upload task panicked")?;
        let second_result = second_upload.await.context("second upload task panicked")?;
        let outcomes = [first_result, second_result];
        let admitted_count = outcomes.iter().filter(|result| result.is_ok()).count();
        let conflict_count = outcomes
            .iter()
            .filter(|result| matches!(result, Err(ApiError::DuplicateContent { .. })))
            .count();

        assert_eq!(
            admitted_count, 1,
            "exactly one concurrent upload must be admitted: {outcomes:?}",
        );
        assert_eq!(
            conflict_count, 1,
            "the duplicate upload must return a typed duplicate_content conflict \
             (code=\"duplicate_content\", carrying existingDocumentId — not a bare \
             ApiError::Conflict, which callers can't distinguish from any other 409): \
             {outcomes:?}",
        );

        let summaries = content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after concurrent uploads")?;
        assert_eq!(summaries.len(), 1, "the admitted operational document must remain visible");
        let summary = &summaries[0];
        assert!(summary.active_revision.is_none(), "unpublished source revision leaked");
        assert!(summary.head.as_ref().is_some_and(|head| head.readable_revision_id.is_none()), "unpublished readable identity leaked");
        assert!(summary.file_name.is_none(), "unpublished source filename leaked");
        assert!(summary.source_access.is_none(), "unpublished source access leaked");
        let active_count: i64 = sqlx::query_scalar("select count(*) from content_document where library_id=$1 and document_state='active' and deleted_at is null")
            .bind(fixture.library_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(active_count, 1, "the owning library must retain exactly one admitted document");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_inline_upload_admits_background_ingest_job() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let admission = fixture
            .state
            .application_services
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
            ironrag_backend::infra::repositories::content_chunk_repository::list_chunks_by_revision(
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
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list authoritative document summaries after inline upload")?;
        assert_eq!(summaries.len(), 1, "accepted work must remain on the operational surface");
        assert!(summaries[0].active_revision.is_none(), "unpublished source revision leaked");
        assert!(summaries[0].head.as_ref().is_some_and(|head| head.readable_revision_id.is_none()), "unpublished readable identity leaked");
        assert!(summaries[0].file_name.is_none(), "unpublished source filename leaked");
        assert!(summaries[0].source_access.is_none(), "unpublished source access leaked");
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
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_single_page_web_ingest_admits_only_the_seed_page()
-> Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;
    web_ingest_support::enable_loopback_test_transport(&mut fixture.state);
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let seed_url = server.url("/seed");
        let web_policy = ironrag_backend::shared::web::ingest::default_web_ingest_policy();
        let run = fixture
            .state
            .application_services
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
                    request_surface: "internal".to_string(),
                    idempotency_key: None,
                },
            )
            .await
            .context("failed to submit single-page web ingest run")?;

        assert_eq!(run.mode, "single_page");
        assert_eq!(run.run_state, "accepted");

        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list single-page web ingest pages")?;
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, seed_url);
        assert_eq!(pages[0].candidate_state, "eligible");
        assert!(pages[0].document_id.is_none());
        assert!(pages[0].result_revision_id.is_none());

        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after single-page web ingest admission")?;
        assert!(documents.is_empty());

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_lifecycle_tracks_append_replace_delete_and_mutation_item_states()
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
            .application_services
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
        let base_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:mutation-base",
                    "Base Revision",
                    Some("file:///base.txt"),
                ),
                "base.txt",
            )
            .await
            .context("failed to create base revision")?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: Some(base_revision.id),
                    readable_revision_id: Some(base_revision.id),
                    publishing_mutation_item_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote base head")?;

        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now() where revision_id=$1")
            .bind(base_revision.id).execute(&fixture.state.persistence.postgres).await?;

        let replace_command = || ReplaceInlineMutationCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id: document.id,
            idempotency_key: Some("authoritative-replace".to_string()),
            requested_by_principal_id: Some(principal.id),
            request_surface: "rest".to_string(),
            source_identity: None,
            visibility_intent_id: None,
            file_name: "replacement.txt".to_string(),
            title: None,
            language_code: None,
            source_uri: None,
            document_hint: None,
            mime_type: Some("text/plain".to_string()),
            file_bytes: b"Replacement content that must stay pending until ingest finishes."
                .to_vec(),
        };
        let replace_admission = fixture
            .state
            .application_services
            .content
            .replace_inline_mutation(&fixture.state, replace_command())
            .await
            .context("failed to admit authoritative replace mutation")?;
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
            .application_services
            .ingest
            .get_job_handle_by_mutation_id(&fixture.state, replace_mutation_id)
            .await
            .context("failed to load replace ingest job handle")?
            .context("replace mutation must enqueue an ingest job")?;
        assert_eq!(replace_job_handle.job.queue_state, "queued");
        assert_eq!(replace_job_handle.job.knowledge_revision_id, Some(replace_revision_id));

        let repeated_replace_admission = fixture
            .state
            .application_services
            .content
            .replace_inline_mutation(&fixture.state, replace_command())
            .await
            .context("failed to replay authoritative replace mutation")?;
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
            .application_services
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
            .application_services
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
        let runtime_execution_id = Uuid::now_v7();
        repositories::runtime_repository::create_runtime_execution(
            &fixture.state.persistence.postgres,
            &repositories::runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: query_execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "ui",
                contract_name: "query_answer",
                contract_version: "content-lifecycle-test",
                lifecycle_state: RuntimeLifecycleState::Completed.as_str(),
                active_stage: None,
                turn_budget: 1,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await
        .context("failed to insert runtime execution for delete cleanup regression")?;
        repositories::query_repository::create_execution(
            &fixture.state.persistence.postgres,
            &repositories::query_repository::NewQueryExecution {
                execution_id: query_execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                conversation_id: query_conversation_id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Which facts came from the base revision?",
                failure_code: None,
            },
        )
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
            .application_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some("authoritative-delete".to_string()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .context("failed to admit authoritative delete mutation")?;
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
            .application_services
            .ops
            .get_async_operation(&fixture.state, delete_operation_id)
            .await
            .context("failed to reload delete async operation")?;
        assert_eq!(delete_operation.status.as_str(), "ready");

        let head_after_delete = fixture
            .state
            .application_services
            .content
            .get_document_head(&fixture.state, document.id)
            .await
            .context("failed to load head after delete")?
            .context("document head missing after delete")?;
        assert_eq!(head_after_delete.active_revision_id, None);
        assert_eq!(head_after_delete.readable_revision_id, None);
        assert_eq!(head_after_delete.readable_revision_id, None);
        assert_eq!(delete_item.base_revision_id,Some(base_revision.id),"audit lineage stays on the accepted item, never a public deleted head");
        assert_eq!(head_after_delete.latest_mutation_id, Some(delete_mutation_id));

        let replace_admission_after_delete = fixture
            .state
            .application_services
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
            .application_services
            .ops
            .get_async_operation(&fixture.state, replace_async_operation_id)
            .await
            .context("failed to reload superseded replace async operation")?;
        assert_eq!(replace_async_operation.status.as_str(), "failed");
        assert_eq!(replace_async_operation.failure_code.as_deref(), Some("document_deleted"));
        let replace_job_handle_after_delete = fixture
            .state
            .application_services
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
        assert_eq!(knowledge_document.readable_revision_id, None);
        assert!(knowledge_document.deleted_at.is_some());

        let active_documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list active documents after delete")?;
        assert!(
            active_documents.iter().all(|summary| summary.document.id != document.id),
            "deleted document must not appear in authoritative active document listings"
        );

        let all_documents = fixture
            .state
            .application_services
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
            .application_services
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
            .application_services
            .ops
            .get_library_state_snapshot(&fixture.state, fixture.library_id)
            .await
            .context("failed to refresh ops snapshot after delete")?;
        assert_eq!(ops_snapshot.state.readable_document_count, 0);
        assert_eq!(ops_snapshot.state.failed_document_count, 0);

        let knowledge_summary = fixture
            .state
            .application_services
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
            .update_document_pointers(DocumentPointersUpdate {
                document_id: document.id,
                document_state: "active",
                active_revision_id: Some(base_revision.id),
                readable_revision_id: Some(base_revision.id),
                latest_revision_no: Some(i64::from(base_revision.revision_number)),
                title: Some("base.txt"),
                deleted_at: None,
            })
            .await
            .context("failed to force stale active knowledge projection before repeated delete")?
            .context("forced stale active knowledge projection missing")?;
        assert_eq!(repaired_projection.document_state, "active");
        let leaked_documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after forcing stale active knowledge projection")?;
        assert!(
            leaked_documents.iter().all(|summary| summary.document.id != document.id),
            "canonical membership must deny stale deleted projections even before repair"
        );
        let deleted_documents = fixture.state.application_services.content
            .list_documents_with_deleted(&fixture.state, fixture.library_id, true).await?;
        let deleted_summary = deleted_documents.iter()
            .find(|summary| summary.document.id == document.id)
            .context("includeDeleted must retain the authoritative operational row")?;
        assert_eq!(deleted_summary.document.document_state, "deleted");
        assert!(deleted_summary.file_name.is_none(), "deleted source filename leaked");
        assert!(deleted_summary.active_revision.is_none(), "deleted source revision leaked");
        assert!(deleted_summary.source_access.is_none(), "deleted source access leaked");

        let repeated_delete = fixture
            .state
            .application_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some("authoritative-delete".to_owned()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await;
        let repeated_delete =
            repeated_delete.context("failed to replay authoritative delete mutation")?;
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
        assert_eq!(healed_knowledge_document.readable_revision_id, None);
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
            .application_services
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
            .application_services
            .content
            .list_mutations(&fixture.state, fixture.library_id)
            .await
            .context("failed to list library mutations after repeated delete")?;
        assert_eq!(
            library_mutations.iter().filter(|mutation| mutation.operation_kind == "delete").count(),
            1,
            "repeated delete must reuse the authoritative delete mutation"
        );

        let append_after_delete = fixture
            .state
            .application_services
            .content
            .append_inline_mutation(
                &fixture.state,
                AppendInlineMutationCommand {
                    base_revision_id: base_revision.id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    idempotency_key: Some(format!("append-after-delete-{}", Uuid::now_v7())),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "internal".to_string(),
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
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_delete_succeeds_when_post_commit_cleanup_fails() -> Result<()> {
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
            .application_services
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
        let base_revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:delete-cleanup-base",
                "Delete Cleanup Base Revision",
                Some("file:///delete-cleanup.txt"),
            ),
            "delete-cleanup.txt",
        )
        .await
        .context("failed to create delete cleanup revision")?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    publishing_mutation_item_id: content_lifecycle_support::publication_item(
                        &fixture.state.persistence.postgres,
                        base_revision.id,
                        "delete-cleanup.txt",
                    )
                    .await?,
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
            .application_services
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
            .application_services
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
            .application_services
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
/// Reproduces the authoritative-cleanup bug observed on the prod Wiki library:
/// 27 nodes / 25 edges survived after every backing document was soft-deleted,
/// because evidence rows for previously-deleted documents kept inflating
/// `support_count` via the unfiltered recalculation. Asserts the authoritative
/// fix on three fronts:
///
/// 1. Library-wide orphan sweep: evidence rows pointing at any
///    `document_state = 'deleted'` document are pruned during the next
///    delete, not just rows for the explicit doc.
/// 2. Active-document filter on `support_count`: orphan evidence does not
///    keep a node alive even before the orphan sweep runs.
/// 3. `runtime_graph_\u{63}anonical_summary` rows for pruned targets are
///    removed (no FK cascade exists on that table).
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_content_delete_drops_orphan_runtime_graph_state() -> Result<()> {
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
            .application_services
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
        let stranded_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    stranded_document.id,
                    "upload",
                    "sha256:graph-stranded",
                    "Stranded Doc",
                    Some("file:///stranded.txt"),
                ),
                "stranded.txt",
            )
            .await
            .context("failed to create stranded revision")?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: stranded_document.id,
                    active_revision_id: Some(stranded_revision.id),
                    readable_revision_id: Some(stranded_revision.id),
                    publishing_mutation_item_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .context("failed to promote stranded head")?;

        let current_document = fixture
            .state
            .application_services
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
        let current_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    current_document.id,
                    "upload",
                    "sha256:graph-current",
                    "Current Doc",
                    Some("file:///current.txt"),
                ),
                "current.txt",
            )
            .await
            .context("failed to create current revision")?;
        fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: current_document.id,
                    active_revision_id: Some(current_revision.id),
                    readable_revision_id: Some(current_revision.id),
                    publishing_mutation_item_id: None,
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
        let entity_node = repositories::upsert_runtime_graph_node(pool, repositories::RuntimeGraphNodeInput {
    library_id: fixture.library_id,
    identity_key: "entity:shared-artifact",
    label: "Shared Artifact",
    node_type: "artifact",
    document_id: None,
    aliases_json: serde_json::json!([]),
    summary: Some("Shared artifact across two documents"),
    metadata_json: serde_json::json!({}),
    support_count: 2,
    projection_version,
})
        .await
        .context("failed to seed entity node")?;
        let stranded_doc_node = repositories::upsert_runtime_graph_node(pool, repositories::RuntimeGraphNodeInput {
    library_id: fixture.library_id,
    identity_key: &format!("document:{}", stranded_document.id),
    label: "stranded.txt",
    node_type: "document",
    document_id: Some(stranded_document.id),
    aliases_json: serde_json::json!([]),
    summary: Some("Stranded document node"),
    metadata_json: serde_json::json!({}),
    support_count: 1,
    projection_version,
})
        .await
        .context("failed to seed stranded document node")?;
        let current_doc_node = repositories::upsert_runtime_graph_node(pool, repositories::RuntimeGraphNodeInput {
    library_id: fixture.library_id,
    identity_key: &format!("document:{}", current_document.id),
    label: "current.txt",
    node_type: "document",
    document_id: Some(current_document.id),
    aliases_json: serde_json::json!([]),
    summary: Some("Current document node"),
    metadata_json: serde_json::json!({}),
    support_count: 1,
    projection_version,
})
        .await
        .context("failed to seed current document node")?;

        let stranded_edge = repositories::upsert_runtime_graph_edge(pool, repositories::RuntimeGraphEdgeInput {
    library_id: fixture.library_id,
    from_node_id: stranded_doc_node.id,
    to_node_id: entity_node.id,
    relation_type: "mentions",
    identity_key: &format!("edge:document-{}:entity", stranded_document.id),
    summary: Some("Stranded mention"),
    weight: Some(1.0),
    support_count: 1,
    metadata_json: serde_json::json!({}),
    projection_version,
})
        .await
        .context("failed to seed stranded edge")?;
        let current_edge = repositories::upsert_runtime_graph_edge(pool, repositories::RuntimeGraphEdgeInput {
    library_id: fixture.library_id,
    from_node_id: current_doc_node.id,
    to_node_id: entity_node.id,
    relation_type: "mentions",
    identity_key: &format!("edge:document-{}:entity", current_document.id),
    summary: Some("Current mention"),
    weight: Some(1.0),
    support_count: 1,
    metadata_json: serde_json::json!({}),
    projection_version,
})
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

        // Seed authoritative summaries for the entity node and one of the edges.
        // No FK cascade exists, so without the targeted cleanup these would
        // outlive their target rows.
        sqlx::query(
            "insert into runtime_graph_\u{63}anonical_summary (
                workspace_id, library_id, target_kind, target_id,
                summary_text, confidence_status, support_count, source_truth_version
            ) values ($1, $2, 'node', $3, 'entity summary', 'confident', 2, 1)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(entity_node.id)
        .execute(pool)
        .await
        .context("failed to seed entity authoritative summary")?;
        sqlx::query(
            "insert into runtime_graph_\u{63}anonical_summary (
                workspace_id, library_id, target_kind, target_id,
                summary_text, confidence_status, support_count, source_truth_version
            ) values ($1, $2, 'edge', $3, 'stranded edge summary', 'confident', 1, 1)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(stranded_edge.id)
        .execute(pool)
        .await
        .context("failed to seed stranded edge authoritative summary")?;

        // Mark the stranded document as already soft-deleted, simulating a
        // failed prior cleanup. This leaves its evidence rows in place — the
        // authoritative fix must sweep them on the next delete in this library.
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
            "test setup must leave stranded evidence rows for the authoritative fix to sweep"
        );

        // Delete the current document via the authoritative service path.
        let delete_admission = fixture
            .state
            .application_services
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

        // Authoritative summary rows for pruned targets are gone too.
        let summary_count: i64 = sqlx::query_scalar(
            "select count(*) from runtime_graph_\u{63}anonical_summary where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(pool)
        .await
        .context("failed to count surviving authoritative summaries")?;
        assert_eq!(
            summary_count, 0,
            "authoritative summaries for pruned nodes/edges must be removed since the table has no FK cascade"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
