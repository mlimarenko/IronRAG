//! Re-admitting the external key of a deleted document.
//!
//! `content_document` releases an external key on delete: its unique index is
//! partial on `document_state = 'active'`. The derived query projection
//! `knowledge_document` carries an unconditional unique index over
//! `(workspace_id, library_id, external_key)`, so a projection row left behind
//! by a deleted document keeps holding the key. Without a reclaim, every later
//! ingest of that key fails with a duplicate-key conflict forever, and a
//! periodic connector loses the same pages on every pass.
//!
//! Both production create paths are covered, because they insert the
//! projection row at two different sites: the service create
//! (`create_document_with_projection`) and the admission create
//! (`admit_content`). The third test covers the materializer, which must not
//! re-insert a projection row for a document whose key another document now
//! owns.

use crate::content_lifecycle_support;

use anyhow::{Context, Result};
use uuid::Uuid;

use ironrag_backend::{
    domains::content::ContentDocument,
    infra::repositories::{
        admission_repository::{
            AdmissionOutcome, ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
            admit_content_with_failpoint,
        },
        content_document_repository, content_knowledge_projection_repository,
    },
    services::content::service::CreateDocumentCommand,
};

use content_lifecycle_support::ContentLifecycleFixture;

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn exact_external_key_http_uses_active_collection_and_authorization() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        let key = "Case /?#&=+";
        let content = &fixture.state.application_services.content;
        let document = content.create_document(&fixture.state, create_command(&fixture, key, "exact.txt")).await?;
        // A newer tombstone with the same key must not hide the active row.
        sqlx::query("insert into content_document (workspace_id, library_id, external_key, document_state, deleted_at, created_at) values ($1, $2, $3, 'deleted', now(), now() + interval '1 second')")
            .bind(fixture.workspace_id).bind(fixture.library_id).bind(key).execute(pool).await?;
        let token = crate::iam_token_support::mint_api_token(pool, Some(fixture.workspace_id), "workspace", "exact-read", &["documents:read"]).await?;
        let foreign_workspace = ironrag_backend::infra::repositories::catalog_repository::create_workspace(pool, "exact-foreign", "Foreign", None).await?;
        let foreign = crate::iam_token_support::mint_api_token(pool, Some(foreign_workspace.id), "workspace", "exact-foreign", &["documents:read"]).await?;
        let denied = crate::iam_token_support::mint_api_token(pool, Some(fixture.workspace_id), "workspace", "exact-denied", &["workspace:read"]).await?;
        let secondary_library: Uuid = sqlx::query_scalar("insert into catalog_library (id, workspace_id, slug, display_name, lifecycle_state, created_at, updated_at) values ($1, $2, 'exact-secondary', 'Exact secondary', 'active', now(), now()) returning id")
            .bind(Uuid::now_v7()).bind(fixture.workspace_id).fetch_one(pool).await?;
        let mut secondary_command = create_command(&fixture, key, "secondary.txt");
        secondary_command.library_id = secondary_library;
        let secondary = content.create_document(&fixture.state, secondary_command).await?;
        let failed = admit_content_with_failpoint(pool, &admission_request(&fixture, "failed-key", "failed-exact"), None).await?.into_bundle();
        sqlx::query("update content_mutation set mutation_state='failed', completed_at=now(), failure_code='fixture_failure' where id=$1")
            .bind(failed.mutation.id).execute(pool).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let base = format!("/content/libraries/{}/documents/by-external-key", fixture.library_id);
        let request = |query: &str, bearer: Option<&str>| {
            let mut builder = Request::get(format!("{base}?externalKey={query}"));
            if let Some(bearer) = bearer { builder = builder.header("authorization", format!("Bearer {bearer}")); }
            builder.body(Body::empty()).expect("request")
        };
        let response = router.clone().oneshot(request("Case%20%2F%3F%23%26%3D%2B", Some(&token.plaintext))).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let item: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 65536).await?)?;
        assert_eq!(item["id"], document.id.to_string());
        assert_eq!(item["externalKey"], key);
        assert!(item.get("visibilityIntentId").is_some_and(serde_json::Value::is_null));
        assert!(item.get("readableRevisionId").is_some_and(serde_json::Value::is_null));
        assert!(item["status"].is_string());
        let collection = router.clone().oneshot(
            Request::get(format!("/content/libraries/{}/documents?ids={}", fixture.library_id, document.id))
                .header("authorization", format!("Bearer {}", token.plaintext)).body(Body::empty())?
        ).await?;
        assert_eq!(collection.status(), StatusCode::OK);
        let page: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(collection.into_body(), 65536).await?)?;
        assert_eq!(item, page["items"][0]);
        assert_eq!(item["status"], "empty");
        let secondary_response = router.clone().oneshot(
            Request::get(format!("/content/libraries/{secondary_library}/documents/by-external-key?externalKey=Case%20%2F%3F%23%26%3D%2B"))
                .header("authorization", format!("Bearer {}", token.plaintext)).body(Body::empty())?
        ).await?;
        assert_eq!(secondary_response.status(), StatusCode::OK);
        let secondary_item: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(secondary_response.into_body(), 65536).await?)?;
        assert_eq!(secondary_item["id"], secondary.id.to_string());
        let failed_response = router.clone().oneshot(request("failed-key", Some(&token.plaintext))).await?;
        assert_eq!(failed_response.status(), StatusCode::OK);
        let failed_item: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(failed_response.into_body(), 65536).await?)?;
        assert_eq!(failed_item["id"], failed.document.id.to_string());
        assert_eq!(failed_item["status"], "failed");
        assert_eq!(failed_item["failureCode"], "fixture_failure");
        for query in ["Case", "case%20%2F%3F%23%26%3D%2B", "missing"] {
            assert_eq!(router.clone().oneshot(request(query, Some(&token.plaintext))).await?.status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(router.clone().oneshot(request("", Some(&token.plaintext))).await?.status(), StatusCode::BAD_REQUEST);
        assert_eq!(router.clone().oneshot(request("%00", Some(&token.plaintext))).await?.status(), StatusCode::BAD_REQUEST);
        assert_eq!(router.clone().oneshot(request("missing", None)).await?.status(), StatusCode::UNAUTHORIZED);
        let denied_existing = router.clone().oneshot(request("Case%20%2F%3F%23%26%3D%2B", Some(&foreign.plaintext))).await?.status();
        let denied_missing = router.clone().oneshot(request("missing", Some(&foreign.plaintext))).await?.status();
        assert_eq!(denied_existing, StatusCode::UNAUTHORIZED);
        assert_eq!(denied_existing, denied_missing);
        let denied_existing = router.clone().oneshot(request("Case%20%2F%3F%23%26%3D%2B", Some(&denied.plaintext))).await?.status();
        let denied_missing = router.clone().oneshot(request("missing", Some(&denied.plaintext))).await?.status();
        assert_eq!(denied_existing, StatusCode::UNAUTHORIZED);
        assert_eq!(denied_existing, denied_missing);
        content.delete_document(&fixture.state, document.id).await?;
        assert_eq!(router.clone().oneshot(request("Case%20%2F%3F%23%26%3D%2B", Some(&token.plaintext))).await?.status(), StatusCode::NOT_FOUND);
        let reclaimed = content.create_document(&fixture.state, create_command(&fixture, key, "reclaimed.txt")).await?;
        let response = router.oneshot(request("Case%20%2F%3F%23%26%3D%2B", Some(&token.plaintext))).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let item: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 65536).await?)?;
        assert_ne!(reclaimed.id, document.id);
        assert_eq!(item["id"], reclaimed.id.to_string());
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

/// Every projection row holding `external_key` in this library, so a test can
/// assert both that the key moved and that it is held exactly once.
async fn projection_holders(
    fixture: &ContentLifecycleFixture,
    external_key: &str,
) -> Result<Vec<Uuid>> {
    sqlx::query_scalar::<_, Uuid>(
        "select document_id
         from knowledge_document
         where workspace_id = $1 and library_id = $2 and external_key = $3
         order by document_id",
    )
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(external_key)
    .fetch_all(&fixture.state.persistence.postgres)
    .await
    .context("failed to read projection rows for the external key")
}

fn create_command(
    fixture: &ContentLifecycleFixture,
    external_key: &str,
    file_name: &str,
) -> CreateDocumentCommand {
    CreateDocumentCommand {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        external_key: Some(external_key.to_string()),
        file_name: Some(file_name.to_string()),
        created_by_principal_id: None,
        parent_external_key: None,
    }
}

/// Replay the sequence that wedged the connector: publish a document under a
/// key, delete it, then publish a fresh one under the same key. Returns the
/// two documents so a caller can assert the second is genuinely new rather
/// than the first revived.
async fn republish_after_delete(
    fixture: &ContentLifecycleFixture,
    external_key: &str,
) -> Result<(ContentDocument, ContentDocument)> {
    let content = &fixture.state.application_services.content;
    let first = content
        .create_document(&fixture.state, create_command(fixture, external_key, "first.txt"))
        .await
        .context("failed to create the first document")?;
    content
        .delete_document(&fixture.state, first.id)
        .await
        .context("failed to delete the first document")?;
    let second = content
        .create_document(&fixture.state, create_command(fixture, external_key, "second.txt"))
        .await
        .context("re-creating a deleted document's external key must succeed")?;
    Ok((first, second))
}

/// Mirrors what a periodic connector sends: a new-target admission carrying
/// the source's stable external key and one revision.
fn admission_request(
    fixture: &ContentLifecycleFixture,
    external_key: &str,
    idempotency_key: &str,
) -> ContentAdmissionRequest {
    ContentAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        operation_kind: "upload".to_string(),
        requested_by_principal_id: None,
        request_surface: "rest".to_string(),
        idempotency_key: Some(idempotency_key.to_string()),
        source_identity: Some("neutral-source:v1".to_string()),
        target: ContentAdmissionTarget::New {
            external_key: Some(external_key.to_string()),
            file_name: Some("synced.txt".to_string()),
            parent_external_key: None,
        },
        revision: Some(RevisionAdmission {
            content_source_kind: "upload".to_string(),
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                file_name: "synced.txt".to_owned(),
                checksum: format!(
                    "sha256:{}",
                    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(idempotency_key.as_bytes()))
                ),
                mime_type: "text/plain".to_string(),
                byte_size: 64,
                title: Some("Synced page".to_string()),
                language_code: Some("en".to_string()),
                source_uri: Some(format!("upload://{idempotency_key}")),
                document_hint: None,
            },
            visibility_intent_id: None,
            base_revision_id: None,
            storage_key: Some(format!("storage/{idempotency_key}")),
        }),
        parent_async_operation_id: None,
        priority: 100,
    }
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn service_create_reclaims_the_external_key_of_a_deleted_document() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let external_key = format!("reclaim-service-{}", Uuid::now_v7().simple());
        let (first, second) = republish_after_delete(&fixture, &external_key).await?;

        assert_ne!(
            second.id, first.id,
            "reclaiming a key must create a new document, not revive the deleted one",
        );
        assert_eq!(
            projection_holders(&fixture, &external_key).await?,
            vec![second.id],
            "the projection key must be held exactly once, by the new document",
        );

        // Only the derived projection row is reclaimed. The authoritative
        // tombstone stays, so the delete remains auditable.
        let first_row = content_document_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            first.id,
        )
        .await?
        .context("the deleted document's authoritative row must survive the reclaim")?;
        assert_eq!(
            first_row.document_state, "deleted",
            "reclaiming the key must not change the deleted document's authoritative state",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn admission_reclaims_the_external_key_of_a_deleted_document() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let external_key = format!("reclaim-admission-{}", Uuid::now_v7().simple());

        let first = match admit_content_with_failpoint(
            postgres,
            &admission_request(&fixture, &external_key, "sync-pass-1"),
            None,
        )
        .await
        .context("failed to admit the first document")?
        {
            AdmissionOutcome::Created(bundle) | AdmissionOutcome::Replayed(bundle) => {
                bundle.document
            }
        };

        fixture
            .state
            .application_services
            .content
            .delete_document(&fixture.state, first.id)
            .await
            .context("failed to delete the admitted document")?;

        // The next connector pass re-sends the same source page.
        let second = match admit_content_with_failpoint(
            postgres,
            &admission_request(&fixture, &external_key, "sync-pass-2"),
            None,
        )
        .await
        .context("re-admitting a deleted document's external key must succeed")?
        {
            AdmissionOutcome::Created(bundle) | AdmissionOutcome::Replayed(bundle) => {
                bundle.document
            }
        };

        assert_ne!(
            second.id, first.id,
            "a re-admitted key must produce a new document, not revive the deleted one",
        );
        assert_eq!(
            projection_holders(&fixture, &external_key).await?,
            vec![second.id],
            "the projection key must be held exactly once, by the re-admitted document",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn materializing_a_reclaimed_document_does_not_resurrect_its_projection() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let external_key = format!("reclaim-materialize-{}", Uuid::now_v7().simple());
        let (first, second) = republish_after_delete(&fixture, &external_key).await?;

        // Promote/repair paths re-materialize a document from its head without
        // checking whether its projection row was reclaimed. Rebuilding the
        // tombstone here would take the key back from the live document and
        // move the conflict loop into the repair path.
        let outcome =
            content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
                postgres,
                first.id,
            )
            .await
            .context("re-materializing a reclaimed document must not fail")?;
        assert_eq!(
            outcome,
            content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized,
            "re-materializing a reclaimed document must report a settled projection",
        );
        assert_eq!(
            projection_holders(&fixture, &external_key).await?,
            vec![second.id],
            "re-materializing the deleted document must not take the key back",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
