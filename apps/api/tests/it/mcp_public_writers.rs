use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::content_lifecycle_support::ContentLifecycleFixture;
use ironrag_backend::{
    app::state::AppState,
    domains::iam::PrincipalKind,
    infra::repositories::{admission_repository::VisibilityAdmissionRequest, iam_repository},
    interfaces::http::{
        auth::{AuthContext, AuthGrant, AuthTokenKind},
        mcp::{
            MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER,
            handle_diagnostics_jsonrpc,
        },
    },
    services::content::service::CreateDocumentCommand,
};

struct PublicWriterClient<'a> {
    fixture: &'a ContentLifecycleFixture,
    auth: AuthContext,
    library: String,
}

impl<'a> PublicWriterClient<'a> {
    async fn create(fixture: &'a ContentLifecycleFixture) -> Result<Self> {
        let pg = &fixture.state.persistence.postgres;
        let principal =
            iam_repository::create_principal(pg, "api_token", "MCP public writers", None).await?;
        let permissions = ["document_write", "library_write", "operation_read"];
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: principal.id,
            parent_principal_id: None,
            workspace_id: Some(fixture.workspace_id),
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: permissions.iter().map(|permission| (*permission).to_owned()).collect(),
            grants: permissions
                .into_iter()
                .map(|permission| AuthGrant {
                    id: Uuid::now_v7(),
                    resource_kind: "workspace".to_owned(),
                    resource_id: fixture.workspace_id,
                    permission_kind: permission.to_owned(),
                    workspace_id: Some(fixture.workspace_id),
                    library_id: None,
                    document_id: None,
                })
                .collect(),
            workspace_memberships: vec![],
            visible_workspace_ids: [fixture.workspace_id].into_iter().collect(),
            is_system_admin: false,
            system_role: None,
        };
        let library = sqlx::query_scalar("select w.slug || '/' || l.slug from catalog_library l join catalog_workspace w on w.id=l.workspace_id where l.id=$1")
            .bind(fixture.library_id).fetch_one(pg).await?;
        Ok(Self { fixture, auth, library })
    }

    async fn call(&self, name: &str, args: Value) -> Result<Value> {
        call_diagnostics(&self.auth, &self.fixture.state, name, args).await
    }

    async fn document(&self) -> Result<Uuid> {
        Ok(self
            .fixture
            .state
            .application_services
            .content
            .create_document(
                &self.fixture.state,
                CreateDocumentCommand {
                    workspace_id: self.fixture.workspace_id,
                    library_id: self.fixture.library_id,
                    external_key: None,
                    file_name: Some("source.txt".to_owned()),
                    created_by_principal_id: Some(self.auth.principal_id),
                    parent_external_key: None,
                },
            )
            .await?
            .id)
    }

    fn replacement(&self, document_id: Uuid) -> Value {
        json!({"library":self.library, "documentId":document_id, "mode":"replace",
            "idempotencyKey":"public-replace", "replacementFileName":"source.txt",
            "replacementContentBase64":STANDARD.encode(b"replacement"),
            "replacementMimeType":"text/plain", "title":null, "languageCode":"en",
            "sourceUri":"https://example.invalid/source", "documentHint":"hint"})
    }
}

async fn call_diagnostics(
    auth: &AuthContext,
    state: &AppState,
    name: &str,
    args: Value,
) -> Result<Value> {
    let initialize = Request::post("/v1/mcp/diagnostics")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc":"2.0", "id":"init", "method":"initialize",
            "params":{"protocolVersion":MCP_PROTOCOL_VERSION, "capabilities":{},
                "clientInfo":{"name":"public-writers-regression", "version":"1"}}})
            .to_string(),
        ))?;
    let initialized =
        handle_diagnostics_jsonrpc(auth.clone(), State(state.clone()), initialize).await;
    assert_eq!(initialized.status(), StatusCode::OK);
    let session =
        initialized.headers().get(MCP_SESSION_HEADER).context("session")?.to_str()?.to_owned();
    let request = Request::post("/v1/mcp/diagnostics")
        .header("content-type", "application/json")
        .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
        .header(MCP_SESSION_HEADER, session)
        .body(Body::from(
            json!({"jsonrpc":"2.0", "id":"writer", "method":"tools/call",
            "params":{"name":name, "arguments":args}})
            .to_string(),
        ))?;
    let response = handle_diagnostics_jsonrpc(auth.clone(), State(state.clone()), request).await;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 64 * 1024).await?)?)
}

fn successful_content(response: &Value) -> Result<Value> {
    assert_ne!(response["result"]["isError"], true, "public MCP writer: {response}");
    response["result"].get("structuredContent").cloned().context("structured content")
}

async fn owner_snapshot(fixture: &ContentLifecycleFixture) -> Result<Value> {
    let pg = &fixture.state.persistence.postgres;
    let mut snapshot = serde_json::Map::new();
    for table in [
        "content_document",
        "content_revision",
        "content_mutation",
        "content_mutation_item",
        "ops_async_operation",
        "ingest_job",
        "content_document_head",
    ] {
        let rows: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select coalesce(jsonb_agg(to_jsonb(t) order by to_jsonb(t)::text),'[]'::jsonb) from {table} t")))
            .fetch_one(pg).await?;
        snapshot.insert(table.to_owned(), rows);
    }
    Ok(Value::Object(snapshot))
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_mcp_bound_replace_replays_original_bundle_and_rejects_full_conflicts()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let client = PublicWriterClient::create(&fixture).await?;
        let document = client.document().await?;
        let other = client.document().await?;
        let descriptor = ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", hex::encode(Sha256::digest(b"replacement"))),
            byte_size: 11, file_name: "source.txt".to_owned(), mime_type: "text/plain".to_owned(),
            title: None, language_code: Some("en".to_owned()), source_uri: Some("https://example.invalid/source".to_owned()),
            document_hint: Some("hint".to_owned()),
        };
        let intent = fixture.state.application_services.content.admit_visibility_intent(&fixture.state,
            VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id: document,
                requested_by_principal_id: Some(client.auth.principal_id), request_surface: "rest".to_owned(),
                intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                    idempotency_key: "public-intent".to_owned(), source_identity: "opaque-source-not-a-byte-hash".to_owned(),
                    materialization_token: None, replacement: descriptor,
                },
            }).await?.into_bundle();
        let intent_id = intent.item.id;
        let mut payload = client.replacement(document);
        payload["visibilityIntentId"] = json!(intent_id);
        let before = owner_snapshot(&fixture).await?;
        let mut foreign = client.auth.clone();
        foreign.workspace_id = Some(Uuid::now_v7());
        foreign.grants.clear();
        foreign.visible_workspace_ids.clear();
        let denied = call_diagnostics(&foreign, &fixture.state, "create_document_revision", payload.clone()).await?;
        assert_eq!(denied["result"]["isError"], true, "foreign scope: {denied}");
        assert_eq!(owner_snapshot(&fixture).await?, before);
        for (field, value) in [
            ("title", json!("changed")), ("languageCode", json!("ru")),
            ("sourceUri", Value::Null), ("documentHint", Value::Null),
            ("replacementFileName", json!("other.txt")),
            ("replacementMimeType", json!("text/markdown")),
            ("replacementContentBase64", json!(STANDARD.encode(b"changed"))),
            ("documentId", json!(other)),
        ] {
            let mut invalid = payload.clone(); invalid[field] = value;
            let response = client.call("create_document_revision", invalid).await?;
            assert_eq!(response["result"]["isError"], true, "{field}: {response}");
            assert_eq!(owner_snapshot(&fixture).await?, before, "rejected {field} changed owners");
        }
        let receipt = successful_content(&client.call("create_document_revision", payload.clone()).await?)?;
        let accepted = owner_snapshot(&fixture).await?;
        let replay = successful_content(&client.call("create_document_revision", payload.clone()).await?)?;
        assert_eq!(receipt, replay, "busy exact replay must keep original operation");
        assert_eq!(owner_snapshot(&fixture).await?, accepted);
        for field in ["title", "languageCode", "sourceUri", "documentHint"] {
            let mut invalid = payload.clone(); invalid[field] = json!("changed");
            assert_eq!(client.call("create_document_revision", invalid).await?["result"]["isError"], true);
            assert_eq!(owner_snapshot(&fixture).await?, accepted);
        }
        // The intent source identity is immutable and supplied by canonical
        // admission, not independently synthesized from the MCP payload bytes.
        let bound_source: String = sqlx::query_scalar("select m.source_identity from content_mutation m join content_mutation_reference r on r.id=m.id where r.idempotency_key='mcp:replace:public-replace' and r.workspace_id=$1 and r.library_id=$2")
            .bind(fixture.workspace_id).bind(fixture.library_id)
            .fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(bound_source, "opaque-source-not-a-byte-hash");
        // Seed a complete failed terminal bundle. This proves terminal replay,
        // not READY processing or provider integration.
        let pg = &fixture.state.persistence.postgres;
        let operation_id: Uuid = serde_json::from_value(receipt["operationId"].clone())?;
        let mutation_id: Uuid = sqlx::query_scalar("select subject_id from ops_async_operation where id=$1")
            .bind(operation_id).fetch_one(pg).await?;
        sqlx::query("update content_mutation set mutation_state='failed', completed_at=now(), failure_code='fixture_failure' where id=$1")
            .bind(mutation_id).execute(pg).await?;
        sqlx::query("update content_mutation_item set item_state='failed' where mutation_id=$1")
            .bind(mutation_id).execute(pg).await?;
        sqlx::query("update ingest_job set queue_state='failed' where mutation_id=$1")
            .bind(mutation_id).execute(pg).await?;
        sqlx::query("update ops_async_operation set status='failed', completed_at=now() where id=$1")
            .bind(operation_id).execute(pg).await?;
        let terminal = owner_snapshot(&fixture).await?;
        let replay = successful_content(&client.call("create_document_revision", payload).await?)?;
        assert_eq!(replay["operationId"], receipt["operationId"]);
        assert_eq!(replay["status"], "failed");
        assert_eq!(owner_snapshot(&fixture).await?, terminal);
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_mcp_upload_replay_validates_metadata_instead_of_weak_lookup()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let client = PublicWriterClient::create(&fixture).await?;
        let payload = json!({"library":client.library, "idempotencyKey":"public-upload",
            "documents":[{"fileName":"source.txt", "body":"c", "title":"ab"}]});
        let receipt = successful_content(&client.call("create_documents", payload.clone()).await?)?;
        let before = owner_snapshot(&fixture).await?;
        let replay = successful_content(&client.call("create_documents", payload.clone()).await?)?;
        assert_eq!(receipt, replay);
        assert_eq!(owner_snapshot(&fixture).await?, before);
        // The retired concatenating hash saw title="ab", bytes="c" and
        // title="a", bytes="bc" as identical. Its weak shortcut returned the
        // accepted receipt instead of asking atomic admission to reject it.
        let mut collision = payload.clone();
        collision["documents"][0]["title"] = json!("a");
        collision["documents"][0]["body"] = json!("bc");
        let response = client.call("create_documents", collision).await?;
        assert_eq!(
            response["result"]["isError"], true,
            "ambiguous old payload identity must conflict: {response}"
        );
        assert_eq!(owner_snapshot(&fixture).await?, before);
        for (field, value) in [
            ("title", json!("changed")),
            ("body", json!("different")),
            ("fileName", json!("different.txt")),
            ("mimeType", json!("text/markdown")),
        ] {
            let mut changed = payload.clone();
            changed["documents"][0][field] = value;
            let response = client.call("create_documents", changed).await?;
            assert_eq!(response["result"]["isError"], true, "{field}: {response}");
            assert_eq!(owner_snapshot(&fixture).await?, before);
        }
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_mcp_modes_reject_present_null_and_retired_policy() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let client = PublicWriterClient::create(&fixture).await?;
        let document = client.document().await?;
        let before = owner_snapshot(&fixture).await?;
        for mode in ["append", "replace", "invalid"] {
            let fields: &[&str] = if mode == "append" {
                &[
                    "replacementFileName",
                    "replacementContentBase64",
                    "replacementMimeType",
                    "title",
                    "languageCode",
                    "sourceUri",
                    "documentHint",
                    "visibilityIntentId",
                ]
            } else {
                &["baseRevisionId", "appendedText"]
            };
            for field in fields.iter().copied().chain(["visibilityPolicy", "visibility_policy"]) {
                let mut invalid =
                    json!({"library":client.library, "documentId":document, "mode":mode});
                invalid[field] = Value::Null;
                let response = client.call("create_document_revision", invalid).await?;
                assert_eq!(response["result"]["isError"], true, "{mode} {field}: {response}");
            }
        }
        assert_eq!(owner_snapshot(&fixture).await?, before);
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}
