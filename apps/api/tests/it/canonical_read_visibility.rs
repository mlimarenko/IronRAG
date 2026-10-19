use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use ironrag_backend::{
    infra::repositories::{
        admission_repository::{
            self, ContentAdmissionBundle, ContentAdmissionRequest, ContentAdmissionTarget,
            RevisionAdmission, VisibilityAdmissionRequest,
        },
        content_fingerprint_repository, content_metadata_search_repository, iam_repository,
    },
    interfaces::http::{
        auth::AuthContext,
        content::{
            self,
            source_download::{SourceDownloadQuery, download_document_source},
        },
        mcp::{
            MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER,
            handle_diagnostics_jsonrpc,
        },
    },
};
use ironrag_contracts::documents::{CreateVisibilityIntentRequest, ReplacementIdentity};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::content_lifecycle_support::ContentLifecycleFixture;

async fn published_summary_a(fixture: &ContentLifecycleFixture) -> Result<ContentAdmissionBundle> {
    let bundle = admission_repository::admit_content_with_failpoint(
        &fixture.state.persistence.postgres,
        &request(fixture, "summary-a", b"restricted A bytes").await?,
        None,
    )
    .await?
    .into_bundle();
    let revision_id = publish(fixture, &bundle, "restricted A bytes").await?;
    sqlx::query("update content_document_head set document_summary=$2 where document_id=$1 and readable_revision_id=$3")
        .bind(bundle.document.id).bind("restricted A summary must not escape").bind(revision_id)
        .execute(&fixture.state.persistence.postgres).await?;
    Ok(bundle)
}

async fn summary_replacement(
    fixture: &ContentLifecycleFixture,
    document_id: Uuid,
) -> Result<ContentAdmissionRequest> {
    let mut replacement = request(fixture, "summary-c", b"public C bytes").await?;
    replacement.operation_kind = "replace".to_owned();
    replacement.target = ContentAdmissionTarget::Existing { document_id };
    let revision = replacement.revision.as_mut().context("replacement revision")?;
    revision.content_source_kind = "replace".to_owned();
    revision.replacement.file_name = "current-c.txt".to_owned();
    Ok(replacement)
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn document_detail_never_attaches_retained_a_summary_to_newly_published_c() -> Result<()> {
    use std::time::Duration;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let a = published_summary_a(&fixture).await?;
        let document_id = a.document.id;
        let auth = auth(&fixture).await?;
        let initial = Box::pin(content::get_document(
            auth.clone(), State(fixture.state.clone()), Path(document_id),
        )).await?.0;
        assert_eq!(initial.head.context("initial head")?.document_summary.as_deref(),
            Some("restricted A summary must not escape"));
        let (reached, release) = fixture.state.application_services.content
            .pause_next_document_detail_read(document_id).await;
        let state = fixture.state.clone();
        let reader_auth = auth.clone();
        let reader = tokio::spawn(async move {
            Box::pin(content::get_document(reader_auth, State(state), Path(document_id))).await
        });
        let mut reader = ironrag_backend::shared::cancellation::AbortOnDrop::new(reader);
        tokio::time::timeout(Duration::from_secs(10), reached).await
            .context("document reader did not retain A's head")??;
        let mut c_request = summary_replacement(&fixture, document_id).await?;
        let intent = admission_repository::admit_visibility_intent_with_failpoint(
            pg,
            &VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id,
                requested_by_principal_id: None, request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "summary-withdraw-a".to_owned(),
                    source_identity: "canonical-read-source".to_owned(), materialization_token: None,
                    replacement: c_request.revision.as_ref().context("C replacement")?.replacement.clone(),
                },
            }, None,
        ).await?.into_bundle();
        c_request.revision.as_mut().context("C revision")?.visibility_intent_id =
            Some(intent.receipt.visibility_intent_id);
        let c = admission_repository::admit_content_with_failpoint(pg, &c_request, None).await?.into_bundle();
        let c_id = publish(&fixture, &c, "public C bytes").await?;
        sqlx::query("update content_document_head set document_summary='public C summary' where document_id=$1 and readable_revision_id=$2")
            .bind(document_id).bind(c_id).execute(pg).await?;
        release.send(()).map_err(|_| anyhow::anyhow!("document read gate disappeared"))?;
        let response = tokio::time::timeout(Duration::from_secs(10), reader.handle_mut().context("document read task missing")?).await
            .context("document reader did not finish")?.context("document reader panicked")?;
        assert_eq!(response.expect_err("C must not authorize retained A summary/item").into_response().status(), StatusCode::NOT_FOUND);
        let current = Box::pin(content::get_document(auth, State(fixture.state.clone()), Path(document_id))).await?.0;
        assert_eq!(current.active_revision.as_ref().map(|revision| revision.id), Some(c_id));
        assert_eq!(current.file_name.as_deref(), Some("current-c.txt"));
        let head = current.head.as_ref().context("C head")?;
        assert_eq!(head.readable_revision_id, Some(c_id));
        assert_eq!(head.document_summary.as_deref(), Some("public C summary"));
        assert!(!serde_json::to_string(&current)?.contains("restricted A summary must not escape"));
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn changing_publication_owner_discards_the_previous_revision_summary() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let a = published_summary_a(&fixture).await?;
        let c_request = summary_replacement(&fixture, a.document.id).await?;
        let c = admission_repository::admit_content_with_failpoint(
            &fixture.state.persistence.postgres, &c_request, None,
        ).await?.into_bundle();
        let c_id = publish(&fixture, &c, "public C bytes").await?;
        let head = ironrag_backend::infra::repositories::content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres, a.document.id,
        ).await?.context("published C head")?;
        assert_eq!(head.readable_revision_id, Some(c_id));
        assert_eq!(head.document_summary, None, "C with no generated summary must not inherit A's summary");
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

async fn cached_execution(
    fixture: &ContentLifecycleFixture,
    revision_id: Uuid,
) -> Result<(Uuid, Uuid, Uuid)> {
    use ironrag_backend::infra::{
        knowledge_rows::KnowledgeContextBundleRow,
        repositories::{query_repository, runtime_repository},
    };
    let pg = &fixture.state.persistence.postgres;
    let conversation = query_repository::create_conversation(
        pg,
        &query_repository::NewQueryConversation {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            created_by_principal_id: None,
            title: Some("canonical cache"),
            conversation_state: "active",
            request_surface: "ui",
        },
        100,
    )
    .await?;
    let execution_id = Uuid::now_v7();
    let runtime_id = Uuid::now_v7();
    let bundle_id = Uuid::now_v7();
    runtime_repository::create_runtime_execution(
        pg,
        &runtime_repository::NewRuntimeExecution {
            id: runtime_id,
            owner_kind: "query_execution",
            owner_id: execution_id,
            task_kind: "query_answer",
            surface_kind: "rest",
            contract_name: "query_answer",
            contract_version: "1",
            lifecycle_state: "completed",
            active_stage: None,
            turn_budget: 4,
            turn_count: 4,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            parent_execution_id: None,
        },
    )
    .await?;
    query_repository::create_execution(
        pg,
        &query_repository::NewQueryExecution {
            execution_id,
            context_bundle_id: bundle_id,
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            conversation_id: conversation.id,
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id: runtime_id,
            query_text: "read cached source",
            failure_code: None,
        },
    )
    .await?;
    let response = query_repository::create_turn(
        pg,
        &query_repository::NewQueryTurn {
            conversation_id: conversation.id,
            turn_kind: "assistant",
            author_principal_id: None,
            content_text: "restricted A cached answer",
            execution_id: Some(execution_id),
        },
    )
    .await?;
    query_repository::update_execution(
        pg,
        execution_id,
        &query_repository::UpdateQueryExecution {
            request_turn_id: None,
            response_turn_id: Some(response.id),
        },
    )
    .await?;
    let now = chrono::Utc::now();
    fixture
        .state
        .context_store
        .upsert_bundle(&KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_owned(),
            bundle_strategy: "canonical-read".to_owned(),
            requested_mode: "mix".to_owned(),
            resolved_mode: "mix".to_owned(),
            selected_fact_ids: vec![],
            verification_state: "verified".to_owned(),
            verification_warnings: serde_json::json!([]),
            freshness_snapshot: serde_json::json!({}),
            candidate_summary: serde_json::json!({"answerDisposition":"factual_ready"}),
            assembly_diagnostics: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        })
        .await?;
    let chunk_id: Uuid =
        sqlx::query_scalar("select chunk_id from knowledge_chunk where revision_id=$1 limit 1")
            .bind(revision_id)
            .fetch_one(pg)
            .await?;
    query_repository::append_chunk_references(
        pg,
        execution_id,
        &[query_repository::NewQueryChunkReference { chunk_id, rank: 1, score: 1.0 }],
    )
    .await?;
    sqlx::query(
        "insert into knowledge_bundle_chunk (bundle_id,chunk_id,library_id,rank,score,created_at)
        values ($1,$2,$3,1,1.0,now())",
    )
    .bind(bundle_id)
    .bind(chunk_id)
    .bind(fixture.library_id)
    .execute(pg)
    .await?;
    Ok((execution_id, conversation.id, bundle_id))
}

async fn request(
    fixture: &ContentLifecycleFixture,
    key: &str,
    bytes: &[u8],
) -> Result<ContentAdmissionRequest> {
    let checksum = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    let storage_key = fixture
        .state
        .content_storage
        .persist_revision_source(
            fixture.workspace_id,
            fixture.library_id,
            "read-family.txt",
            &checksum,
            bytes,
        )
        .await?;
    Ok(ContentAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        operation_kind: "upload".to_owned(),
        requested_by_principal_id: None,
        request_surface: "rest".to_owned(),
        idempotency_key: Some(key.to_owned()),
        source_identity: Some("canonical-read-source".to_owned()),
        target: ContentAdmissionTarget::New {
            external_key: Some(key.to_owned()),
            file_name: Some("read-family.txt".to_owned()),
            parent_external_key: None,
        },
        revision: Some(RevisionAdmission {
            content_source_kind: "upload".to_owned(),
            replacement: ReplacementIdentity {
                file_name: "read-family.txt".to_owned(),
                checksum,
                mime_type: "text/plain".to_owned(),
                byte_size: i64::try_from(bytes.len())?,
                title: Some(key.to_owned()),
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
            visibility_intent_id: None,
            base_revision_id: None,
            storage_key: Some(storage_key),
        }),
        parent_async_operation_id: None,
        priority: 100,
    })
}

async fn publish(
    fixture: &ContentLifecycleFixture,
    bundle: &ContentAdmissionBundle,
    text: &str,
) -> Result<Uuid> {
    let pg = &fixture.state.persistence.postgres;
    let revision_id = bundle.revision.as_ref().context("revision")?.id;
    let item_id = bundle.item.as_ref().context("published item")?.id;
    sqlx::query("update content_mutation set mutation_state='applied' where id=$1")
        .bind(bundle.mutation.id)
        .execute(pg)
        .await?;
    sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
        .bind(item_id)
        .execute(pg)
        .await?;
    sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now(),
        vector_state='ready',vector_ready_at=now(),graph_state='ready',graph_ready_at=now(),normalized_text=$2 where revision_id=$1")
        .bind(revision_id).bind(text).execute(pg).await?;
    ironrag_backend::infra::repositories::content_document_head_repository::upsert_document_head(
        pg,
        &ironrag_backend::infra::repositories::content_document_head_repository::NewContentDocumentHead {
            document_id: bundle.document.id,
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            publishing_mutation_item_id: Some(item_id),
            latest_mutation_id: Some(bundle.mutation.id),
            latest_successful_attempt_id: None,
        },
    ).await?;
    sqlx::query(
        "update knowledge_document set active_revision_id=$2,readable_revision_id=$2,
        document_state='active' where document_id=$1",
    )
    .bind(bundle.document.id)
    .bind(revision_id)
    .execute(pg)
    .await?;
    sqlx::query("insert into knowledge_structured_revision (revision_id,workspace_id,library_id,document_id,
        preparation_state,normalization_profile,source_format,block_count,chunk_count,typed_fact_count,
        outline_json,prepared_at,updated_at) values ($1,$2,$3,$4,'prepared','canonical-read','text',0,1,0,'{}',now(),now())")
        .bind(revision_id).bind(fixture.workspace_id).bind(fixture.library_id).bind(bundle.document.id).execute(pg).await?;
    let chunk = ironrag_backend::infra::repositories::content_chunk_repository::create_chunk(
        pg,
        &ironrag_backend::infra::repositories::content_chunk_repository::NewContentChunk {
            revision_id,
            chunk_index: 0,
            start_offset: 0,
            end_offset: i32::try_from(text.len())?,
            token_count: None,
            normalized_text: text,
            text_checksum: &format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes()))),
            occurred_at: None,
            occurred_until: None,
        },
    )
    .await?;
    sqlx::query("insert into knowledge_chunk (chunk_id,workspace_id,library_id,document_id,revision_id,
        chunk_index,content_text,normalized_text,chunk_state) values ($1,$2,$3,$4,$5,0,$6,$6,'ready')")
        .bind(chunk.id).bind(fixture.workspace_id).bind(fixture.library_id).bind(bundle.document.id)
        .bind(revision_id).bind(text).execute(pg).await?;
    assert_eq!(
        content_fingerprint_repository::get_current_revision_id(pg, bundle.document.id).await?,
        Some(revision_id)
    );
    Ok(revision_id)
}

async fn auth(fixture: &ContentLifecycleFixture) -> Result<AuthContext> {
    let principal = iam_repository::create_principal(
        &fixture.state.persistence.postgres,
        "api_token",
        "canonical read test",
        None,
    )
    .await?;
    let permissions = [
        "document_read",
        "document_write",
        "library_read",
        "knowledge_read",
        "operation_read",
        "query_read",
    ];
    Ok(crate::iam_token_support::scoped_workspace_auth(
        principal.id,
        fixture.workspace_id,
        &permissions,
    ))
}

async fn mcp_read(
    fixture: &ContentLifecycleFixture,
    auth: &AuthContext,
    document_id: Uuid,
) -> Result<serde_json::Value> {
    let initialize = Request::post("/v1/mcp/diagnostics").header("content-type", "application/json")
        .body(Body::from(serde_json::json!({"jsonrpc":"2.0","id":"init","method":"initialize",
            "params":{"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"canonical-read","version":"1"}}}).to_string()))?;
    let initialized =
        handle_diagnostics_jsonrpc(auth.clone(), State(fixture.state.clone()), initialize).await;
    assert_eq!(initialized.status(), StatusCode::OK);
    let session =
        initialized.headers().get(MCP_SESSION_HEADER).context("MCP session")?.to_str()?.to_owned();
    let call = Request::post("/v1/mcp/diagnostics")
        .header("content-type", "application/json")
        .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
        .header(MCP_SESSION_HEADER, session)
        .body(Body::from(
            serde_json::json!({"jsonrpc":"2.0","id":"read","method":"tools/call",
            "params":{"name":"read_document","arguments":{"documentId":document_id,"mode":"full"}}})
            .to_string(),
        ))?;
    let response =
        handle_diagnostics_jsonrpc(auth.clone(), State(fixture.state.clone()), call).await;
    serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 128 * 1024).await?)
        .context("MCP response")
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn canonical_read_family_denies_old_a_with_preaccepted_b_and_permits_bound_c() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let a = admission_repository::admit_content_with_failpoint(
            pg,
            &request(&fixture, "restricted-a", b"restricted A bytes").await?,
            None,
        )
        .await?
        .into_bundle();
        let document_id = a.document.id;
        let a_id = publish(&fixture, &a, "restricted A bytes").await?;
        let auth = auth(&fixture).await?;
        let source = |revision_id| SourceDownloadQuery { revision_id, representation: None };
        let initial = Box::pin(download_document_source(
            auth.clone(),
            State(fixture.state.clone()),
            Path(document_id),
            Query(source(Some(a_id))),
        ))
        .await?;
        assert_eq!(
            axum::body::to_bytes(initial.into_body(), 128 * 1024).await?.as_ref(),
            b"restricted A bytes"
        );
        let initial_mcp = mcp_read(&fixture, &auth, document_id).await?;
        assert_ne!(initial_mcp["result"]["isError"], true, "{initial_mcp}");
        assert_eq!(initial_mcp["result"]["structuredContent"]["content"], "restricted A bytes");

        let mut b_request = request(&fixture, "preaccepted-b", b"pending B bytes").await?;
        b_request.operation_kind = "edit".to_owned();
        b_request.target = ContentAdmissionTarget::Existing { document_id };
        b_request.revision.as_mut().context("B")?.content_source_kind = "edit".to_owned();
        let b = admission_repository::admit_content_with_failpoint(pg, &b_request, None)
            .await?
            .into_bundle();
        let b_id = b.revision.as_ref().context("B revision")?.id;
        let mut c_request = request(&fixture, "published-c", b"public C bytes").await?;
        c_request.operation_kind = "replace".to_owned();
        c_request.target = ContentAdmissionTarget::Existing { document_id };
        c_request.revision.as_mut().context("C")?.content_source_kind = "replace".to_owned();
        c_request.revision.as_mut().context("C")?.replacement.file_name = "safe-c.txt".to_owned();
        let intent = admission_repository::admit_visibility_intent_with_failpoint(
            pg,
            &VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id,
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "read-denial".to_owned(),
                    source_identity: "canonical-read-source".to_owned(),
                    materialization_token: None,
                    replacement: c_request
                        .revision
                        .as_ref()
                        .context("C replacement")?
                        .replacement
                        .clone(),
                },
            },
            None,
        )
        .await?
        .into_bundle();
        assert_eq!(
            content_fingerprint_repository::get_current_revision_id(pg, document_id).await?,
            None
        );
        for selected in [None, Some(a_id), Some(b_id)] {
            let response = Box::pin(download_document_source(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(source(selected)),
            ))
            .await;
            assert_eq!(
                response.expect_err("old source must be denied").into_response().status(),
                StatusCode::NOT_FOUND
            );
        }
        assert!(
            content::get_document(auth.clone(), State(fixture.state.clone()), Path(document_id))
                .await
                .is_err()
        );
        assert!(
            content::get_document_head(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id)
            )
            .await
            .is_err()
        );
        assert!(
            content::list_revisions(auth.clone(), State(fixture.state.clone()), Path(document_id))
                .await?
                .0
                .is_empty()
        );
        assert!(
            content::list_chunks(
                auth.clone(),
                State(fixture.state.clone()),
                Query(serde_json::from_value(serde_json::json!({"documentId":document_id}))?)
            )
            .await
            .is_err()
        );
        assert!(
            content::get_document_prepared_segments(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(serde_json::from_value(serde_json::json!({}))?)
            )
            .await
            .is_err()
        );
        assert!(
            content::get_document_technical_facts(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(serde_json::from_value(serde_json::json!({}))?)
            )
            .await
            .is_err()
        );
        let hidden = mcp_read(&fixture, &auth, document_id).await?;
        assert_eq!(hidden["result"]["isError"], true, "{hidden}");
        assert!(!hidden.to_string().contains("restricted A bytes"));
        assert!(
            content_metadata_search_repository::search_document_metadata_rows(
                pg,
                fixture.library_id,
                "restricted-a",
                10
            )
            .await?
            .is_empty()
        );

        // Reader fixtures seed successful-but-invisible B independently of the
        // publication-writer suite. No head or projection is promoted here.
        sqlx::query(
            "update content_mutation set mutation_state='applied',completed_at=now() where id=$1",
        )
        .bind(b.mutation.id)
        .execute(pg)
        .await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
            .bind(b.item.as_ref().context("B item")?.id)
            .execute(pg)
            .await?;
        assert_eq!(
            content_fingerprint_repository::get_current_revision_id(pg, document_id).await?,
            None
        );
        c_request.revision.as_mut().context("bound C")?.visibility_intent_id =
            Some(intent.receipt.visibility_intent_id);
        let c = admission_repository::admit_content_with_failpoint(pg, &c_request, None)
            .await?
            .into_bundle();
        let c_id = publish(&fixture, &c, "public C bytes").await?;
        for selected in [None, Some(c_id)] {
            let response = Box::pin(download_document_source(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(source(selected)),
            ))
            .await?;
            assert!(response.headers()["content-disposition"].to_str()?.contains("safe-c.txt"));
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 128 * 1024).await?.as_ref(),
                b"public C bytes"
            );
        }
        assert!(
            Box::pin(download_document_source(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(source(Some(a_id)))
            ))
            .await
            .is_err()
        );
        let public = mcp_read(&fixture, &auth, document_id).await?;
        assert_ne!(public["result"]["isError"], true, "{public}");
        assert_eq!(public["result"]["structuredContent"]["content"], "public C bytes");
        assert_eq!(
            public["result"]["structuredContent"]["latestRevisionId"],
            serde_json::json!(c_id)
        );
        assert!(!public.to_string().contains("restricted A bytes"));
        assert!(
            content::get_document(auth.clone(), State(fixture.state.clone()), Path(document_id))
                .await
                .is_ok()
        );
        assert!(
            content::get_document_head(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id)
            )
            .await
            .is_ok()
        );
        let revisions =
            content::list_revisions(auth.clone(), State(fixture.state.clone()), Path(document_id))
                .await?;
        assert_eq!(revisions.0.len(), 1);
        assert_eq!(revisions.0[0].id, c_id);
        assert!(
            content_metadata_search_repository::search_document_metadata_rows(
                pg,
                fixture.library_id,
                "read-family.txt",
                10
            )
            .await?
            .is_empty()
        );
        assert_eq!(
            content_metadata_search_repository::search_document_metadata_rows(
                pg,
                fixture.library_id,
                "safe-c.txt",
                10
            )
            .await?
            .len(),
            1
        );
        sqlx::query(
            "update content_document set source_file_name='restricted-renamed-a.txt' where id=$1",
        )
        .bind(document_id)
        .execute(pg)
        .await?;
        assert_eq!(
            content_fingerprint_repository::get_current_source_file_name(pg, document_id, c_id)
                .await?
                .as_deref(),
            Some("safe-c.txt")
        );
        assert!(
            content_metadata_search_repository::search_document_metadata_rows(
                pg,
                fixture.library_id,
                "restricted-renamed-a.txt",
                10
            )
            .await?
            .is_empty()
        );
        let patch = content::patch_document_metadata(
            auth.clone(),
            State(fixture.state.clone()),
            Path(document_id),
            axum::Json(serde_json::from_value(
                serde_json::json!({"documentHint":"changed protected metadata"}),
            )?),
        )
        .await;
        assert_eq!(
            patch
                .expect_err("protected metadata cannot rewrite its admitted descriptor")
                .into_response()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            content_fingerprint_repository::get_current_revision_id(pg, document_id).await?,
            Some(c_id)
        );
        let chunks = content::list_chunks(
            auth.clone(),
            State(fixture.state.clone()),
            Query(serde_json::from_value(serde_json::json!({"documentId":document_id}))?),
        )
        .await?;
        assert_eq!(chunks.0.len(), 1);
        let chunks_json = serde_json::to_string(&chunks.0)?;
        assert!(chunks_json.contains("public C bytes"));
        assert!(!chunks_json.contains("restricted A bytes"));
        assert!(
            content::get_document_prepared_segments(
                auth.clone(),
                State(fixture.state.clone()),
                Path(document_id),
                Query(serde_json::from_value(serde_json::json!({}))?)
            )
            .await
            .is_ok()
        );
        assert!(
            content::get_document_technical_facts(
                auth,
                State(fixture.state.clone()),
                Path(document_id),
                Query(serde_json::from_value(serde_json::json!({}))?)
            )
            .await
            .is_ok()
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn canonical_cache_revalidates_all_sources_after_deterministic_intent_barrier() -> Result<()>
{
    use ironrag_backend::infra::repositories::{
        catalog_repository, query_repository, query_result_cache_repository,
    };
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let a = admission_repository::admit_content_with_failpoint(
            pg,
            &request(&fixture, "cached-a", b"restricted A bytes").await?,
            None,
        )
        .await?
        .into_bundle();
        let a_id = publish(&fixture, &a, "restricted A bytes").await?;
        let (execution_id, conversation_id, _bundle_id) = cached_execution(&fixture, a_id).await?;
        let auth = auth(&fixture).await?;
        assert!(
            ironrag_backend::interfaces::http::query::get_execution(
                auth.clone(),
                State(fixture.state.clone()),
                Path(execution_id)
            )
            .await
            .is_ok()
        );
        assert!(
            query_result_cache_repository::execution_sources_are_current(
                pg,
                execution_id,
                fixture.workspace_id,
                fixture.library_id
            )
            .await?
        );
        let request_turn = query_repository::create_turn(
            pg,
            &query_repository::NewQueryTurn {
                conversation_id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "replay",
                execution_id: None,
            },
        )
        .await?;
        let generation =
            catalog_repository::get_library_source_truth_version(pg, fixture.library_id).await?;
        let cache_key = "canonical-read-cache";
        query_result_cache_repository::upsert_query_result_cache_winner(
            pg,
            &query_result_cache_repository::UpsertQueryResultCacheInput {
                cache_key,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                source_execution_id: execution_id,
                expected_source_truth_version: generation,
                readable_content_fingerprint: "canonical",
                graph_projection_version: 1,
                graph_topology_generation: 1,
                binding_fingerprint: "provider-free",
                ttl_seconds: 300,
            },
        )
        .await?
        .context("cache winner")?;
        let replay = query_result_cache_repository::CreateQueryExecutionReplayInput {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            conversation_id,
            request_turn_id: request_turn.id,
            source_execution_id: execution_id,
            expected_source_truth_version: generation,
            cache_key,
            ttl_seconds: 300,
        };
        assert!(
            query_result_cache_repository::create_query_execution_replay(pg, &replay)
                .await?
                .is_some()
        );

        let replacement = request(&fixture, "cached-c", b"public C bytes").await?;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let reader_pg = pg.clone();
        let reader_barrier = barrier.clone();
        let workspace_id = fixture.workspace_id;
        let library_id = fixture.library_id;
        let reader = tokio::spawn(async move {
            let before = query_result_cache_repository::execution_sources_are_current(
                &reader_pg,
                execution_id,
                workspace_id,
                library_id,
            )
            .await?;
            reader_barrier.wait().await;
            reader_barrier.wait().await;
            let after = query_result_cache_repository::execution_sources_are_current(
                &reader_pg,
                execution_id,
                workspace_id,
                library_id,
            )
            .await?;
            Ok::<_, sqlx::Error>((before, after))
        });
        barrier.wait().await;
        let admission = admission_repository::admit_visibility_intent_with_failpoint(
            pg,
            &VisibilityAdmissionRequest {
                workspace_id,
                library_id,
                document_id: a.document.id,
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "cache-denial".to_owned(),
                    source_identity: "canonical-read-source".to_owned(),
                    materialization_token: None,
                    replacement: replacement
                        .revision
                        .as_ref()
                        .context("replacement")?
                        .replacement
                        .clone(),
                },
            },
            None,
        )
        .await;
        barrier.wait().await;
        admission?;
        assert_eq!(
            reader.await??,
            (true, false),
            "late cache reader must revalidate membership, not reuse its first snapshot"
        );
        // Refresh the row and deliberately use the current generation: freshness
        // and a matching cache key must not substitute for source membership.
        sqlx::query("update query_result_cache set updated_at=now() where cache_key=$1")
            .bind(cache_key)
            .execute(pg)
            .await?;
        let mut fresh = replay;
        fresh.expected_source_truth_version =
            catalog_repository::get_library_source_truth_version(pg, library_id).await?;
        assert!(
            query_result_cache_repository::create_query_execution_replay(pg, &fresh)
                .await?
                .is_none()
        );
        assert!(
            fixture
                .state
                .application_services
                .query
                .get_execution(&fixture.state, execution_id)
                .await
                .is_err()
        );
        let denied = ironrag_backend::interfaces::http::query::get_execution(
            auth,
            State(fixture.state.clone()),
            Path(execution_id),
        )
        .await;
        assert_eq!(
            denied.expect_err("cached HTTP answer must be denied").into_response().status(),
            StatusCode::NOT_FOUND
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}
