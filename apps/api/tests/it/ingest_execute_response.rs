//! IRG-360: `POST /v1/ops/ingest-queue/jobs/{jobId}/execute` answers with the
//! executed job's own record.
//!
//! The handler used to run the job and then compose its response from the
//! global active-queue snapshot, which requires `POLICY_USAGE_READ`. A connector
//! principal holding `library_write` but not `ops_read` was therefore refused
//! *after* the work had succeeded and published. The tests below use exactly
//! that principal and prove the three parts of the contract:
//!
//! 1. the exact endpoint is authorized for it — a queued job without an
//!    admitted mutation origin yields the documented conflict, never 401/403;
//! 2. a successful run returns this job's DTO, carrying the executed state and
//!    no other job from the queue;
//! 3. `GET /v1/ingest/queue` stays closed to it, so the global snapshot policy
//!    is unchanged.

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::state::AppState,
    infra::repositories::iam_repository,
    interfaces::http::{auth::hash_token, authorization::PERMISSION_LIBRARY_WRITE, router},
    services::{
        content::service::{AdmitMutationCommand, CreateDocumentCommand, RevisionAdmission},
        ingest::service::AdmitIngestJobCommand,
    },
    shared::auth_tokens::mint_plaintext_api_token,
};

use crate::{
    content_lifecycle_support::ContentLifecycleFixture, http_response_support::response_json,
    ingest_ai_fixture,
};

const SOURCE_FILE_NAME: &str = "irg360-execute.txt";

fn app(state: &AppState) -> Router {
    Router::new().nest("/v1", router()).with_state(state.clone())
}

async fn rest_post(
    state: &AppState,
    token: &str,
    path: &str,
    payload: Value,
) -> Result<(StatusCode, Value)> {
    let response = app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .with_context(|| format!("failed to build POST {path}"))?,
        )
        .await
        .with_context(|| format!("POST {path} failed"))?;
    let status = response.status();
    Ok((status, response_json(response).await?))
}

async fn rest_get(state: &AppState, token: &str, path: &str) -> Result<(StatusCode, Value)> {
    let response = app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .with_context(|| format!("failed to build GET {path}"))?,
        )
        .await
        .with_context(|| format!("GET {path} failed"))?;
    let status = response.status();
    Ok((status, response_json(response).await?))
}

/// A real API token for the fixture workspace with `library_write` and nothing
/// else: no `ops_read`, so the global queue snapshot is out of its reach.
async fn mint_library_writer(
    fixture: &ContentLifecycleFixture,
    label: &str,
) -> Result<(String, Uuid)> {
    let postgres = &fixture.state.persistence.postgres;
    let plaintext = mint_plaintext_api_token();
    let token = iam_repository::create_api_token(
        postgres,
        Some(fixture.workspace_id),
        label,
        "irg360-execute",
        None,
        None,
    )
    .await
    .with_context(|| format!("failed to create the api token for {label}"))?;
    iam_repository::create_api_token_secret(postgres, token.principal_id, &hash_token(&plaintext))
        .await
        .with_context(|| format!("failed to create the api token secret for {label}"))?;
    // Workspace-scoped requests also check membership, not only grants.
    iam_repository::upsert_workspace_membership(
        postgres,
        fixture.workspace_id,
        token.principal_id,
        "active",
    )
    .await
    .with_context(|| format!("failed to create the workspace membership for {label}"))?;
    iam_repository::create_grant(
        postgres,
        &iam_repository::NewGrant {
            principal_id: token.principal_id,
            resource_kind: "workspace",
            resource_id: fixture.workspace_id,
            permission_kind: PERMISSION_LIBRARY_WRITE,
            granted_by_principal_id: None,
            expires_at: None,
        },
    )
    .await
    .with_context(|| format!("failed to grant library_write for {label}"))?;
    Ok((plaintext, token.principal_id))
}

fn sha256_checksum(text: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
}

/// Admits a `reprocess` mutation that records `principal_id` as its origin, so
/// the resulting job is admitted rather than unattributed — and leaves it
/// queued, because nothing dispatches admission.
async fn admit_queued_reprocess(
    fixture: &ContentLifecycleFixture,
    principal_id: Uuid,
) -> Result<Uuid> {
    let state = &fixture.state;
    let source = "IRG-360 exact job execute response source text.\n";
    let checksum = sha256_checksum(source);
    let document = state
        .application_services
        .content
        .create_document(
            state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some(format!("irg360-execute-{}", Uuid::now_v7().simple())),
                file_name: Some(SOURCE_FILE_NAME.to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await
        .context("failed to create the execute response document")?;
    let storage_key = state
        .content_storage
        .persist_revision_source(
            fixture.workspace_id,
            fixture.library_id,
            SOURCE_FILE_NAME,
            &checksum,
            source.as_bytes(),
        )
        .await
        .context("failed to persist the execute response source bytes")?;
    let admission = state
        .application_services
        .content
        .admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: document.id,
                operation_kind: "reprocess".to_string(),
                enrichment_scope: None,
                idempotency_key: Some(format!("irg360-execute-{}", Uuid::now_v7().simple())),
                requested_by_principal_id: Some(principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(RevisionAdmission {
                    content_source_kind: "upload".to_string(),
                    replacement: ironrag_contracts::documents::ReplacementIdentity {
                        file_name: SOURCE_FILE_NAME.to_string(),
                        checksum,
                        mime_type: "text/plain".to_string(),
                        byte_size: i64::try_from(source.len()).unwrap_or(i64::MAX),
                        title: Some("IRG-360 Execute Response".to_string()),
                        language_code: None,
                        source_uri: Some(format!("upload://{SOURCE_FILE_NAME}")),
                        document_hint: None,
                    },
                    visibility_intent_id: None,
                    base_revision_id: None,
                    storage_key: Some(storage_key),
                }),
                parent_async_operation_id: None,
            },
        )
        .await
        .context("failed to admit the execute response mutation")?;
    admission.job_id.context("the execute response mutation admitted no ingest job")
}

async fn admit_unattributed_job(fixture: &ContentLifecycleFixture, job_kind: &str) -> Result<Uuid> {
    let job = fixture
        .state
        .application_services
        .ingest
        .admit_job(
            &fixture.state,
            AdmitIngestJobCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: job_kind.to_string(),
                priority: 100,
                dedupe_key: None,
                available_at: None,
            },
        )
        .await
        .context("failed to admit the queued job under test")?;
    Ok(job.id)
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn execute_endpoint_is_authorized_for_a_library_writer_without_ops_read() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let (token, _principal_id) =
            mint_library_writer(&fixture, "irg360-execute-authorization").await?;
        let job_id = admit_unattributed_job(&fixture, "content_mutation").await?;

        let path = format!("/v1/ops/ingest-queue/jobs/{job_id}/execute");
        let (status, body) = rest_post(&fixture.state, &token, &path, json!({})).await?;
        // Authorized: the refusal is the documented conflict for a job with no
        // admitted mutation origin, never a missing permission.
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "the exact execute endpoint must be authorized for a library writer: {body:#}",
        );

        let job = fixture
            .state
            .application_services
            .ingest
            .get_job(&fixture.state, job_id)
            .await?;
        assert_eq!(job.queue_state, "queued", "a refused execute must not lease the job");

        // The global active queue still owns its own permission.
        let (queue_status, queue_body) =
            rest_get(&fixture.state, &token, "/v1/ingest/queue").await?;
        assert_eq!(
            queue_status,
            StatusCode::UNAUTHORIZED,
            "the global queue snapshot must stay closed to a writer without ops_read: {queue_body:#}",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn execute_endpoint_returns_the_executed_job_and_not_the_global_queue() -> Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;
    // The pipeline runs against the suite's in-process gateway, so this proves
    // a real admitted run without a network provider call.
    ingest_ai_fixture::configure_ingest_ai(
        &mut fixture.state,
        fixture.workspace_id,
        fixture.library_id,
        "IRG-360 execute response",
    )
    .await?;

    let result = async {
        let (token, principal_id) =
            mint_library_writer(&fixture, "irg360-execute-response").await?;
        let job_id = admit_queued_reprocess(&fixture, principal_id).await?;
        // A second queued job in the same library: the old response was the
        // global queue snapshot, which listed this one too.
        let unrelated_job_id = admit_unattributed_job(&fixture, "graph_refresh").await?;

        let path = format!("/v1/ops/ingest-queue/jobs/{job_id}/execute");
        let (status, body) = rest_post(&fixture.state, &token, &path, json!({})).await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "the executed job must answer with its own record: {body:#}",
        );

        assert_eq!(
            body["job"]["id"],
            json!(job_id),
            "the response must carry the requested job: {body:#}",
        );
        assert_eq!(
            body["job"]["queue_state"],
            json!("completed"),
            "the response must carry the executed state, not the pre-run one: {body:#}",
        );
        assert!(
            body["latestAttempt"].is_object(),
            "the per-job DTO carries the executed attempt: {body:#}",
        );

        // Neither shape nor content of the global active-queue snapshot.
        assert!(
            body.get("items").is_none() && body.get("summary").is_none(),
            "the response must not be an active-queue snapshot: {body:#}",
        );
        let rendered = body.to_string();
        assert!(
            !rendered.contains(&unrelated_job_id.to_string()),
            "another queued job must never appear in the response: {body:#}",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
