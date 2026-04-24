#[path = "support/content_lifecycle_support.rs"]
mod content_lifecycle_support;
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::{Context, Result};
use uuid::Uuid;

use ironrag_backend::{
    infra::repositories::iam_repository,
    interfaces::http::router_support::ApiError,
    services::{
        content::service::{
            AcceptMutationCommand, AdmitMutationCommand, AppendInlineMutationCommand,
            CreateDocumentCommand, EditInlineMutationCommand, PromoteHeadCommand,
            ReplaceInlineMutationCommand, RevisionAdmissionMetadata, UploadInlineDocumentCommand,
        },
        ingest::web::CreateWebIngestRunCommand,
    },
};

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

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
async fn canonical_content_lifecycle_head_promotion_fails_loudly_when_knowledge_sync_breaks()
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
                    external_key: Some(format!("head-sync-failure-doc-{}", Uuid::now_v7())),
                    file_name: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create head sync failure document")?;

        fixture
            .drop_arango_database()
            .await
            .context("failed to drop Arango database before head promotion")?;

        let error = fixture
            .state
            .canonical_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: None,
                    readable_revision_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .expect_err("head promotion must fail when knowledge sync fails");
        assert!(matches!(error, ApiError::InternalMessage(_)));
        assert!(
            error
                .to_string()
                .contains("knowledge document sync failed after canonical head update")
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
            ironrag_backend::infra::repositories::content_repository::list_chunks_by_revision(
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
                    extra_ignore_patterns: Vec::new(),
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
                    file_name: None,
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
            .arango_document_store
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
            .context("failed to force stale active Arango projection before repeated delete")?
            .context("forced stale active Arango projection missing")?;
        assert_eq!(repaired_projection.document_state, "active");
        let leaked_documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after forcing stale active Arango projection")?;
        assert!(
            leaked_documents.iter().any(|summary| summary.document.id == document.id),
            "stale active Arango projection must reproduce the leaked deleted document before repair"
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
            .arango_document_store
            .get_document(document.id)
            .await
            .context("failed to reload healed knowledge document after repeated delete")?
            .context("healed knowledge document missing from arango")?;
        assert_eq!(healed_knowledge_document.document_state, "deleted");
        assert_eq!(healed_knowledge_document.active_revision_id, None);
        assert_eq!(healed_knowledge_document.readable_revision_id, Some(base_revision.id));
        assert!(healed_knowledge_document.deleted_at.is_some());
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
            "repeated delete must heal stale Arango projections and hide the deleted document again"
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
    }
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
            .arango_document_store
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
