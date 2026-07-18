#![cfg(feature = "test-support")]

#[path = "support/content_lifecycle_support.rs"]
mod content_lifecycle_support;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Barrier;
use uuid::Uuid;

use ironrag_backend::infra::repositories::admission_repository::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, ContentAdmissionRequest,
    ContentAdmissionTarget, RevisionAdmission, WebCaptureMaterializationAdmissionRequest,
    WebRunAdmissionRequest, admit_content_with_failpoint,
    admit_web_capture_materialization_with_failpoint, admit_web_run_with_failpoint,
};
use ironrag_backend::services::content::service::{
    AcceptMutationCommand, AdmitMutationCommand, CreateDocumentCommand, CreateMutationItemCommand,
    PromoteHeadCommand,
};
use ironrag_backend::{
    infra::repositories::{content_repository, iam_repository},
    interfaces::http::router_support::ApiError,
};

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

fn content_request(fixture: &ContentLifecycleFixture, key: &str) -> ContentAdmissionRequest {
    ContentAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        operation_kind: "upload".to_string(),
        requested_by_principal_id: None,
        request_surface: "rest".to_string(),
        idempotency_key: Some(key.to_string()),
        source_identity: Some("neutral-source:v1".to_string()),
        target: ContentAdmissionTarget::New {
            external_key: Some(format!("atomic-{key}")),
            file_name: Some("atomic.txt".to_string()),
            parent_external_key: None,
        },
        revision: Some(RevisionAdmission {
            content_source_kind: "upload".to_string(),
            checksum: format!("sha256:{key}"),
            mime_type: "text/plain".to_string(),
            byte_size: 64,
            title: Some("Atomic admission".to_string()),
            language_code: Some("en".to_string()),
            source_uri: Some(format!("upload://{key}")),
            document_hint: None,
            storage_key: Some(format!("storage/{key}")),
        }),
        parent_async_operation_id: None,
        priority: 100,
    }
}

fn delete_mutation_command(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
    principal_id: Uuid,
    idempotency_key: Option<String>,
) -> AdmitMutationCommand {
    AdmitMutationCommand {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        document_id,
        operation_kind: "delete".to_string(),
        idempotency_key,
        requested_by_principal_id: Some(principal_id),
        request_surface: "rest".to_string(),
        source_identity: None,
        revision: None,
        parent_async_operation_id: None,
    }
}

fn web_request(fixture: &ContentLifecycleFixture, key: &str) -> WebRunAdmissionRequest {
    WebRunAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        seed_url: "https://example.invalid/guide".to_string(),
        normalized_seed_url: "https://example.invalid/guide".to_string(),
        mode: "recursive_crawl".to_string(),
        boundary_policy: "same_host".to_string(),
        max_depth: 2,
        max_pages: 20,
        crawl_allow_patterns: serde_json::json!([]),
        crawl_block_patterns: serde_json::json!([]),
        materialization_allow_patterns: serde_json::json!([]),
        materialization_block_patterns: serde_json::json!([]),
        requested_by_principal_id: None,
        request_surface: "rest".to_string(),
        idempotency_key: Some(key.to_string()),
        source_identity: Some("neutral-web-source:v1".to_string()),
    }
}

async fn admission_row_counts(fixture: &ContentLifecycleFixture) -> Result<Vec<i64>> {
    let postgres = &fixture.state.persistence.postgres;
    let mut counts = Vec::new();
    for table in [
        "content_mutation",
        "ops_async_operation",
        "content_document",
        "content_revision",
        "content_mutation_item",
        "ingest_job",
        "content_web_ingest_run",
        "content_web_discovered_page",
    ] {
        counts.push(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "select count(*)::bigint from {table}"
            )))
            .fetch_one(postgres)
            .await
            .with_context(|| format!("failed to count {table}"))?,
        );
    }
    Ok(counts)
}

async fn web_capture_materialization_request(
    fixture: &ContentLifecycleFixture,
    key: &str,
) -> Result<WebCaptureMaterializationAdmissionRequest> {
    let web = admit_web_run_with_failpoint(
        &fixture.state.persistence.postgres,
        &web_request(fixture, key),
        None,
    )
    .await?
    .into_bundle();
    let document = fixture
        .state
        .canonical_services
        .content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some(format!("materialized-{key}")),
                file_name: Some("materialized.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await?;
    let revision = fixture
        .state
        .canonical_services
        .content
        .create_revision(
            &fixture.state,
            revision_command(
                document.id,
                "web_page",
                &format!("sha256:{key}"),
                "Materialized capture",
                Some("https://example.invalid/materialized"),
            ),
        )
        .await?;

    Ok(WebCaptureMaterializationAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        mutation_id: web.mutation.id,
        document_id: document.id,
        revision_id: revision.id,
        requested_by_principal_id: None,
        priority: 100,
    })
}

async fn materialization_owned_row_counts(
    fixture: &ContentLifecycleFixture,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<(i64, i64)> {
    let item_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from content_mutation_item
         where mutation_id = $1
           and document_id = $2
           and result_revision_id = $3",
    )
    .bind(request.mutation_id)
    .bind(request.document_id)
    .bind(request.revision_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    let job_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_job
         where mutation_id = $1
           and knowledge_document_id = $2
           and knowledge_revision_id = $3
           and job_kind = 'content_mutation'",
    )
    .bind(request.mutation_id)
    .bind(request.document_id)
    .bind(request.revision_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    Ok((item_count, job_count))
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn content_admission_rolls_back_every_write_boundary() -> Result<()> {
    for failpoint in [
        AdmissionFailpoint::AfterMutation,
        AdmissionFailpoint::AfterDocument,
        AdmissionFailpoint::AfterAsyncOperation,
        AdmissionFailpoint::AfterRevision,
        AdmissionFailpoint::AfterMutationItem,
        AdmissionFailpoint::AfterJob,
        AdmissionFailpoint::AfterHead,
        AdmissionFailpoint::BeforeCommit,
    ] {
        let fixture = ContentLifecycleFixture::create().await?;
        let baseline = admission_row_counts(&fixture).await?;
        let error = admit_content_with_failpoint(
            &fixture.state.persistence.postgres,
            &content_request(&fixture, &format!("rollback-{failpoint:?}")),
            Some(failpoint),
        )
        .await
        .expect_err("an injected admission failure must abort the transaction");
        assert!(matches!(error, AdmissionError::InjectedFailure(point) if point == failpoint));
        assert_eq!(admission_row_counts(&fixture).await?, baseline, "failed at {failpoint:?}");
        fixture.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn concurrent_content_retries_return_one_complete_graph_and_scope_jobs_structurally()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request = Arc::new(content_request(&fixture, "concurrent-content"));
    let barrier = Arc::new(Barrier::new(16));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let postgres = fixture.state.persistence.postgres.clone();
        let request = Arc::clone(&request);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            admit_content_with_failpoint(&postgres, &request, None).await
        }));
    }

    let mut bundles = Vec::new();
    for task in tasks {
        bundles.push(task.await.context("content admission task panicked")??.into_bundle());
    }
    let first = bundles.first().context("no content admission outcomes")?;
    assert!(bundles.iter().all(|bundle| {
        bundle.mutation.id == first.mutation.id
            && bundle.document.id == first.document.id
            && bundle.revision.as_ref().map(|row| row.id)
                == first.revision.as_ref().map(|row| row.id)
            && bundle.item.as_ref().map(|row| row.id) == first.item.as_ref().map(|row| row.id)
            && bundle.job.as_ref().map(|row| row.id) == first.job.as_ref().map(|row| row.id)
            && bundle.async_operation.id == first.async_operation.id
    }));
    assert!(first.is_complete());

    let counts = admission_row_counts(&fixture).await?;
    assert_eq!(&counts[..6], &[1, 1, 1, 1, 1, 1]);
    let item = first.item.as_ref().context("missing mutation item")?;
    let job = first.job.as_ref().context("missing ingest job")?;
    assert_eq!(job.mutation_id, Some(first.mutation.id));
    assert_eq!(job.mutation_item_id, Some(item.id));
    assert_eq!(
        job.dedupe_key.as_deref(),
        Some(format!("content-mutation:{}:{}", first.mutation.id, item.id).as_str())
    );

    let mut conflicting = (*request).clone();
    conflicting.target = ContentAdmissionTarget::Existing { document_id: Uuid::now_v7() };
    let conflict =
        admit_content_with_failpoint(&fixture.state.persistence.postgres, &conflicting, None)
            .await
            .expect_err("same key for another document must conflict");
    assert!(matches!(conflict, AdmissionError::IdempotencyConflict { .. }));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn repeated_delete_repairs_a_stale_applied_item_before_validating_the_head() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("stale-delete-{}", Uuid::now_v7())),
                    file_name: Some("stale-delete.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        let revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:stale-delete",
                    "Stale delete fixture",
                    Some("upload://stale-delete.txt"),
                ),
            )
            .await?;
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
            .await?;

        let stale_delete = content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "delete".to_string(),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    idempotency_key: None,
                    source_identity: None,
                },
            )
            .await?;
        let stale_item = content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: stale_delete.id,
                    document_id: None,
                    base_revision_id: None,
                    result_revision_id: None,
                    item_state: "applied".to_string(),
                    message: Some("stale applied item".to_string()),
                },
            )
            .await?;

        sqlx::query(
            "update content_document
             set document_state = 'deleted', deleted_at = now()
             where id = $1",
        )
        .bind(document.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "update content_document_head
             set latest_mutation_id = $2
             where document_id = $1",
        )
        .bind(document.id)
        .bind(stale_delete.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let admission = content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: None,
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .context(
                "a repeated delete must repair its stale item instead of returning HTTP 500",
            )?;

        assert_eq!(admission.mutation.id, stale_delete.id);
        let repaired_item = admission.items.first().context("delete item missing after repair")?;
        assert_eq!(repaired_item.id, stale_item.id);
        assert_eq!(repaired_item.document_id, Some(document.id));
        assert_eq!(repaired_item.base_revision_id, Some(revision.id));
        assert_eq!(repaired_item.item_state, "applied");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn existing_target_admission_reports_missing_and_deleted_documents_with_typed_errors()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let missing_document_id = Uuid::now_v7();
        let mut missing_request = content_request(&fixture, "missing-existing-target");
        missing_request.operation_kind = "replace".to_string();
        missing_request.target =
            ContentAdmissionTarget::Existing { document_id: missing_document_id };
        let missing_error = admit_content_with_failpoint(
            &fixture.state.persistence.postgres,
            &missing_request,
            None,
        )
        .await
        .expect_err("a missing target must reject atomic admission");
        assert!(matches!(
            missing_error,
            AdmissionError::TargetDocumentNotFound { document_id }
                if document_id == missing_document_id
        ));

        let deleted_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("deleted-target-{}", Uuid::now_v7())),
                    file_name: Some("deleted-target.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        sqlx::query(
            "update content_document
             set document_state = 'deleted', deleted_at = now()
             where id = $1",
        )
        .bind(deleted_document.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let mut deleted_request = content_request(&fixture, "deleted-existing-target");
        deleted_request.operation_kind = "replace".to_string();
        deleted_request.target =
            ContentAdmissionTarget::Existing { document_id: deleted_document.id };
        let deleted_error = admit_content_with_failpoint(
            &fixture.state.persistence.postgres,
            &deleted_request,
            None,
        )
        .await
        .expect_err("a deleted target must reject atomic admission");
        assert!(matches!(
            deleted_error,
            AdmissionError::TargetDocumentDeleted { document_id }
                if document_id == deleted_document.id
        ));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn invalid_head_reference_is_an_integrity_error_not_a_retryable_conflict() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.canonical_services.content;
        let source_document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("head-source-{}", Uuid::now_v7())),
                    file_name: Some("source.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        let foreign_revision = content
            .create_revision(
                &fixture.state,
                revision_command(
                    source_document.id,
                    "upload",
                    "sha256:foreign-head-reference",
                    "Foreign head reference",
                    Some("upload://foreign-head-reference.txt"),
                ),
            )
            .await?;
        let target_document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("head-target-{}", Uuid::now_v7())),
                    file_name: Some("target.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        sqlx::query(
            "update content_document_head
             set active_revision_id = $2, readable_revision_id = $2
             where document_id = $1",
        )
        .bind(target_document.id)
        .bind(foreign_revision.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let mutation_count_before = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from content_mutation where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;

        let mut request = content_request(&fixture, "invalid-head-reference");
        request.operation_kind = "replace".to_string();
        request.target = ContentAdmissionTarget::Existing { document_id: target_document.id };
        let error =
            admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
                .await
                .expect_err("a foreign head reference must reject atomic admission");
        assert!(matches!(
            error,
            AdmissionError::TargetDocumentHeadIntegrity { document_id }
                if document_id == target_document.id
        ));

        let mutation_count_after = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from content_mutation where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(mutation_count_after, mutation_count_before);
        let target_revision_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from content_revision where document_id = $1",
        )
        .bind(target_document.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(target_revision_count, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn projection_promotion_reports_reference_integrity_instead_of_missing_document() -> Result<()>
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
                    external_key: Some(format!("promotion-integrity-{}", Uuid::now_v7())),
                    file_name: Some("document.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        let head_before =
            content_repository::get_document_head(&fixture.state.persistence.postgres, document.id)
                .await?
                .context("document head missing before invalid promotion")?;

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
                    latest_mutation_id: Some(Uuid::now_v7()),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .expect_err("invalid head ownership must be an integrity error");
        assert!(matches!(error, ApiError::Internal));

        let head_after =
            content_repository::get_document_head(&fixture.state.persistence.postgres, document.id)
                .await?
                .context("document head missing after invalid promotion")?;
        assert_eq!(head_after.active_revision_id, head_before.active_revision_id);
        assert_eq!(head_after.readable_revision_id, head_before.readable_revision_id);
        assert_eq!(head_after.latest_mutation_id, head_before.latest_mutation_id);
        assert_eq!(
            head_after.latest_successful_attempt_id,
            head_before.latest_successful_attempt_id
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn delete_idempotency_key_cannot_move_a_mutation_item_to_another_document() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Delete idempotency principal",
            None,
        )
        .await?;
        let first_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("delete-first-{}", Uuid::now_v7())),
                    file_name: Some("first.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let second_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("delete-second-{}", Uuid::now_v7())),
                    file_name: Some("second.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let second_head_before = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing before delete replay")?;
        let idempotency_key = format!("delete-target-{}", Uuid::now_v7());

        let first_admission = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: first_document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some(idempotency_key.clone()),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await?;
        let original_item =
            first_admission.items.first().context("first delete mutation item missing")?.clone();

        let second_error = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                AdmitMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    document_id: second_document.id,
                    operation_kind: "delete".to_string(),
                    idempotency_key: Some(idempotency_key),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .expect_err("one delete idempotency key must not target two documents");
        assert!(matches!(second_error, ApiError::IdempotencyConflict(_)));

        let original_admission = fixture
            .state
            .canonical_services
            .content
            .get_mutation_admission(&fixture.state, first_admission.mutation.id)
            .await?;
        assert_eq!(original_admission.items.len(), 1);
        assert_eq!(original_admission.items[0].id, original_item.id);
        assert_eq!(original_admission.items[0].document_id, Some(first_document.id));
        let second_document_after = content_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document disappeared after conflicting delete")?;
        assert_eq!(second_document_after.document_state, "active");
        assert!(second_document_after.deleted_at.is_none());
        let second_head_after = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing after conflicting delete")?;
        assert_eq!(second_head_after.active_revision_id, second_head_before.active_revision_id);
        assert_eq!(second_head_after.readable_revision_id, second_head_before.readable_revision_id);
        assert_eq!(second_head_after.latest_mutation_id, second_head_before.latest_mutation_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn fresh_delete_key_on_deleted_document_is_bound_before_replay() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Deleted replay principal",
            None,
        )
        .await?;
        let first_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("deleted-replay-first-{}", Uuid::now_v7())),
                    file_name: Some("first.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let second_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("deleted-replay-second-{}", Uuid::now_v7())),
                    file_name: Some("second.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let second_head_before = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing before keyed replay")?;

        let initial_delete = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                delete_mutation_command(
                    &fixture,
                    first_document.id,
                    principal.id,
                    Some(format!("initial-delete-{}", Uuid::now_v7())),
                ),
            )
            .await?;
        let fresh_key = format!("deleted-replay-key-{}", Uuid::now_v7());
        let keyed_replay = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                delete_mutation_command(
                    &fixture,
                    first_document.id,
                    principal.id,
                    Some(fresh_key.clone()),
                ),
            )
            .await?;

        assert_ne!(keyed_replay.mutation.id, initial_delete.mutation.id);
        assert_eq!(keyed_replay.mutation.idempotency_key.as_deref(), Some(fresh_key.as_str()));
        assert_eq!(
            keyed_replay.items.first().and_then(|item| item.document_id),
            Some(first_document.id)
        );

        let second_error = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                delete_mutation_command(
                    &fixture,
                    second_document.id,
                    principal.id,
                    Some(fresh_key),
                ),
            )
            .await
            .expect_err("the fresh key must remain bound to the deleted document replay");
        assert!(matches!(second_error, ApiError::IdempotencyConflict(_)));

        let second_document_after = content_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document disappeared after keyed replay conflict")?;
        assert_eq!(second_document_after.document_state, "active");
        assert!(second_document_after.deleted_at.is_none());
        let second_head_after = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing after keyed replay conflict")?;
        assert_eq!(second_head_after.active_revision_id, second_head_before.active_revision_id);
        assert_eq!(second_head_after.readable_revision_id, second_head_before.readable_revision_id);
        assert_eq!(second_head_after.latest_mutation_id, second_head_before.latest_mutation_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn concurrent_deleted_replays_claim_one_unbound_item_once() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Concurrent delete repair principal",
            None,
        )
        .await?;
        let first_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("unbound-claim-first-{}", Uuid::now_v7())),
                    file_name: Some("first.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let second_document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("unbound-claim-second-{}", Uuid::now_v7())),
                    file_name: Some("second.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let shared_mutation = fixture
            .state
            .canonical_services
            .content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "delete".to_string(),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    idempotency_key: None,
                    source_identity: None,
                },
            )
            .await?;
        let unbound_item = fixture
            .state
            .canonical_services
            .content
            .create_mutation_item(
                &fixture.state,
                CreateMutationItemCommand {
                    mutation_id: shared_mutation.id,
                    document_id: None,
                    base_revision_id: None,
                    result_revision_id: None,
                    item_state: "pending".to_string(),
                    message: Some("unbound delete repair".to_string()),
                },
            )
            .await?;
        let document_ids = vec![first_document.id, second_document.id];
        sqlx::query(
            "update content_document
             set document_state = 'deleted', deleted_at = now()
             where id = any($1)",
        )
        .bind(&document_ids)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "update content_document_head
             set latest_mutation_id = $1
             where document_id = any($2)",
        )
        .bind(shared_mutation.id)
        .bind(&document_ids)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let (first_claim, second_claim) = tokio::join!(
            content_repository::claim_unbound_mutation_item(
                &fixture.state.persistence.postgres,
                shared_mutation.id,
                unbound_item.id,
                first_document.id,
                None,
                None,
                "pending",
                Some("unbound delete repair"),
            ),
            content_repository::claim_unbound_mutation_item(
                &fixture.state.persistence.postgres,
                shared_mutation.id,
                unbound_item.id,
                second_document.id,
                None,
                None,
                "pending",
                Some("unbound delete repair"),
            ),
        );
        let (winner_document_id, losing_document_id) = match (first_claim?, second_claim?) {
            (Some(claimed), None) => {
                assert_eq!(claimed.document_id, Some(first_document.id));
                (first_document.id, second_document.id)
            }
            (None, Some(claimed)) => {
                assert_eq!(claimed.document_id, Some(second_document.id));
                (second_document.id, first_document.id)
            }
            _ => {
                anyhow::bail!("exactly one concurrent claimant must bind the unbound mutation item")
            }
        };

        let losing_error = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                delete_mutation_command(&fixture, losing_document_id, principal.id, None),
            )
            .await
            .expect_err("the losing document must not move the claimed mutation item");
        assert!(matches!(losing_error, ApiError::IdempotencyConflict(_)));

        let items = content_repository::list_mutation_items(
            &fixture.state.persistence.postgres,
            shared_mutation.id,
        )
        .await?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, unbound_item.id);
        assert_eq!(items[0].document_id, Some(winner_document_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn deleted_replay_rejects_multiple_unbound_items_as_ambiguous() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let principal = iam_repository::create_principal(
            &fixture.state.persistence.postgres,
            "user",
            "Ambiguous delete repair principal",
            None,
        )
        .await?;
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("ambiguous-unbound-{}", Uuid::now_v7())),
                    file_name: Some("document.txt".to_string()),
                    created_by_principal_id: Some(principal.id),
                    parent_external_key: None,
                },
            )
            .await?;
        let mutation = fixture
            .state
            .canonical_services
            .content
            .accept_mutation(
                &fixture.state,
                AcceptMutationCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    operation_kind: "delete".to_string(),
                    requested_by_principal_id: Some(principal.id),
                    request_surface: "rest".to_string(),
                    idempotency_key: None,
                    source_identity: None,
                },
            )
            .await?;
        for message in ["first unbound item", "second unbound item"] {
            let _ = fixture
                .state
                .canonical_services
                .content
                .create_mutation_item(
                    &fixture.state,
                    CreateMutationItemCommand {
                        mutation_id: mutation.id,
                        document_id: None,
                        base_revision_id: None,
                        result_revision_id: None,
                        item_state: "pending".to_string(),
                        message: Some(message.to_string()),
                    },
                )
                .await?;
        }
        sqlx::query(
            "update content_document
             set document_state = 'deleted', deleted_at = now()
             where id = $1",
        )
        .bind(document.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "update content_document_head
             set latest_mutation_id = $2
             where document_id = $1",
        )
        .bind(document.id)
        .bind(mutation.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let error = fixture
            .state
            .canonical_services
            .content
            .admit_mutation(
                &fixture.state,
                delete_mutation_command(&fixture, document.id, principal.id, None),
            )
            .await
            .expect_err("multiple unbound mutation items must be rejected as ambiguous");
        assert!(matches!(error, ApiError::IdempotencyConflict(_)));
        let items = content_repository::list_mutation_items(
            &fixture.state.persistence.postgres,
            mutation.id,
        )
        .await?;
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.document_id.is_none()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn complete_content_replay_repairs_only_a_safely_identifiable_missing_job() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request = content_request(&fixture, "repair-job");
    let created = admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
        .await?
        .into_bundle();
    let job_id = created.job.as_ref().context("missing initial job")?.id;
    sqlx::query("delete from ingest_job where id = $1")
        .bind(job_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

    let replay =
        admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None).await?;
    assert!(matches!(replay, AdmissionOutcome::RepairedLegacy(_)));
    let repaired = replay.into_bundle();
    assert!(repaired.is_complete());
    assert_ne!(repaired.job.as_ref().map(|row| row.id), Some(job_id));
    assert_eq!(repaired.mutation.id, created.mutation.id);
    assert_eq!(repaired.item.as_ref().map(|row| row.id), created.item.as_ref().map(|row| row.id));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_admission_commits_run_seed_and_discovery_job_as_one_replayable_graph() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request = Arc::new(web_request(&fixture, "web-concurrent"));
    let barrier = Arc::new(Barrier::new(12));
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let postgres = fixture.state.persistence.postgres.clone();
        let request = Arc::clone(&request);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            admit_web_run_with_failpoint(&postgres, &request, None).await
        }));
    }

    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.context("web admission task panicked")??);
    }
    let first = outcomes.first().context("no web admission outcomes")?.bundle();
    assert!(first.is_complete());
    assert!(outcomes.iter().all(|outcome| {
        let bundle = outcome.bundle();
        bundle.mutation.id == first.mutation.id
            && bundle.async_operation.id == first.async_operation.id
            && bundle.run.id == first.run.id
            && bundle.seed.id == first.seed.id
            && bundle.job.as_ref().map(|row| row.id) == first.job.as_ref().map(|row| row.id)
    }));
    let counts = admission_row_counts(&fixture).await?;
    assert_eq!(counts[0], 1);
    assert_eq!(counts[1], 1);
    assert_eq!(counts[5], 1);
    assert_eq!(counts[6], 1);
    assert_eq!(counts[7], 1);

    let seed_id = first.seed.id;
    sqlx::query("delete from content_web_discovered_page where id = $1")
        .bind(seed_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
    let repaired =
        admit_web_run_with_failpoint(&fixture.state.persistence.postgres, &request, None).await?;
    assert!(matches!(repaired, AdmissionOutcome::RepairedLegacy(_)));
    assert!(repaired.bundle().is_complete());
    assert_ne!(repaired.bundle().seed.id, seed_id);

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_admission_rolls_back_before_any_post_commit_work_can_observe_it() -> Result<()> {
    for failpoint in [
        AdmissionFailpoint::AfterMutation,
        AdmissionFailpoint::AfterAsyncOperation,
        AdmissionFailpoint::AfterWebRun,
        AdmissionFailpoint::AfterWebSeed,
        AdmissionFailpoint::AfterJob,
        AdmissionFailpoint::BeforeCommit,
    ] {
        let fixture = ContentLifecycleFixture::create().await?;
        let baseline = admission_row_counts(&fixture).await?;
        let error = admit_web_run_with_failpoint(
            &fixture.state.persistence.postgres,
            &web_request(&fixture, &format!("web-rollback-{failpoint:?}")),
            Some(failpoint),
        )
        .await
        .expect_err("an injected web admission failure must abort the transaction");
        assert!(matches!(error, AdmissionError::InjectedFailure(point) if point == failpoint));
        assert_eq!(admission_row_counts(&fixture).await?, baseline, "failed at {failpoint:?}");
        fixture.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_capture_materialization_commits_exact_item_job_and_pending_head_once() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request = web_capture_materialization_request(&fixture, "materialization-commit").await?;

    let created = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await?;
    assert!(matches!(created, AdmissionOutcome::Created(_)));
    let first = created.into_bundle();
    assert!(first.is_complete());
    assert_eq!(first.item.mutation_id, request.mutation_id);
    assert_eq!(first.item.document_id, Some(request.document_id));
    assert_eq!(first.item.result_revision_id, Some(request.revision_id));
    assert_eq!(first.item.item_state, "pending");
    assert_eq!(first.job.mutation_id, Some(request.mutation_id));
    assert_eq!(first.job.mutation_item_id, Some(first.item.id));
    assert_eq!(first.job.knowledge_document_id, Some(request.document_id));
    assert_eq!(first.job.knowledge_revision_id, Some(request.revision_id));
    assert_eq!(first.job.job_kind, "content_mutation");
    assert_eq!(first.job.queue_state, "queued");
    assert_eq!(first.head.latest_mutation_id, Some(request.mutation_id));

    let replay = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await?;
    assert!(matches!(replay, AdmissionOutcome::Replayed(_)));
    let replayed = replay.into_bundle();
    assert_eq!(replayed.item.id, first.item.id);
    assert_eq!(replayed.job.id, first.job.id);
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (1, 1));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_capture_materialization_rolls_back_every_owned_write_without_failing_the_run()
-> Result<()> {
    for failpoint in [
        AdmissionFailpoint::AfterMutationItem,
        AdmissionFailpoint::AfterJob,
        AdmissionFailpoint::AfterHead,
        AdmissionFailpoint::BeforeCommit,
    ] {
        let fixture = ContentLifecycleFixture::create().await?;
        let request = web_capture_materialization_request(
            &fixture,
            &format!("materialization-rollback-{failpoint:?}"),
        )
        .await?;
        let initial_head = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            request.document_id,
        )
        .await?
        .context("materialization document head missing")?;

        let error = admit_web_capture_materialization_with_failpoint(
            &fixture.state.persistence.postgres,
            &request,
            Some(failpoint),
        )
        .await
        .expect_err("injected materialization failure must abort the transaction");
        assert!(matches!(error, AdmissionError::InjectedFailure(point) if point == failpoint));
        assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));
        let final_head = content_repository::get_document_head(
            &fixture.state.persistence.postgres,
            request.document_id,
        )
        .await?
        .context("materialization document head disappeared")?;
        assert_eq!(final_head.latest_mutation_id, initial_head.latest_mutation_id);
        let mutation_state = sqlx::query_scalar::<_, String>(
            "select mutation_state::text from content_mutation where id = $1",
        )
        .bind(request.mutation_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(mutation_state, "accepted");

        fixture.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_capture_materialization_rejects_cross_document_revision_without_partial_rows()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let mut request =
        web_capture_materialization_request(&fixture, "materialization-identity").await?;
    let foreign_document = fixture
        .state
        .canonical_services
        .content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some(format!("foreign-{}", Uuid::now_v7())),
                file_name: Some("foreign.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await?;
    let foreign_revision = fixture
        .state
        .canonical_services
        .content
        .create_revision(
            &fixture.state,
            revision_command(
                foreign_document.id,
                "web_page",
                "sha256:foreign-materialization",
                "Foreign materialization",
                Some("https://example.invalid/foreign"),
            ),
        )
        .await?;
    request.revision_id = foreign_revision.id;

    let error = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await
    .expect_err("cross-document revision must fail exact admission validation");
    assert!(matches!(error, AdmissionError::InvariantViolation(_)));
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_capture_materialization_rejects_a_divergent_revision_projection_without_partial_rows()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request =
        web_capture_materialization_request(&fixture, "materialization-projection").await?;
    sqlx::query(
        "update knowledge_revision
         set checksum = $2
         where revision_id = $1",
    )
    .bind(request.revision_id)
    .bind("sha256:divergent-projection")
    .execute(&fixture.state.persistence.postgres)
    .await?;

    let error = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await
    .expect_err("a divergent revision projection must fail exact admission validation");
    assert!(matches!(error, AdmissionError::InvariantViolation(_)));
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn web_capture_materialization_does_not_admit_new_work_after_its_web_owner_is_terminal()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request = web_capture_materialization_request(&fixture, "materialization-closed").await?;
    let mut transaction = fixture.state.persistence.postgres.begin().await?;
    sqlx::query(
        "update content_web_ingest_run
         set run_state = 'failed',
             completed_at = now(),
             failure_code = 'synthetic_terminal_owner'
         where mutation_id = $1",
    )
    .bind(request.mutation_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update content_mutation
         set mutation_state = 'failed',
             completed_at = now(),
             failure_code = 'synthetic_terminal_owner'
         where id = $1",
    )
    .bind(request.mutation_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let error = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await
    .expect_err("a terminal web owner must not admit a new child ingest");
    assert!(matches!(error, AdmissionError::InvariantViolation(_)));
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));

    fixture.cleanup().await
}
