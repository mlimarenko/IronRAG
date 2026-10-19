use crate::content_lifecycle_support;

use std::sync::Arc;

use anyhow::{Context, Result};
use sha2::Digest as _;
use tokio::sync::Barrier;
use uuid::Uuid;

use ironrag_backend::infra::repositories::admission_repository::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, ContentAdmissionRequest,
    ContentAdmissionTarget, RevisionAdmission, WebCaptureMaterializationAdmissionRequest,
    WebRunAdmissionRequest, admit_content_with_failpoint,
    admit_web_capture_materialization_with_failpoint, admit_web_run_with_failpoint,
};
use ironrag_backend::services::content::service::{
    AdmitMutationCommand, CreateDocumentCommand, PromoteHeadCommand,
};
use ironrag_backend::{
    infra::repositories::{
        content_document_head_repository, content_document_repository, iam_repository,
    },
    interfaces::http::router_support::ApiError,
};

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

async fn seed_upload_without_head(
    fixture: &ContentLifecycleFixture,
    key: &str,
    admitted: bool,
) -> Result<(Uuid, Uuid, String)> {
    let pool = &fixture.state.persistence.postgres;
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let checksum = format!("sha256:{}", hex::encode(sha2::Sha256::digest(b"neutral source")));
    sqlx::query("insert into content_document (id,workspace_id,library_id,external_key,source_file_name,revision_sequence) values ($1,$2,$3,$4,'safe.txt',1)")
        .bind(document_id).bind(fixture.workspace_id).bind(fixture.library_id).bind(key).execute(pool).await?;
    sqlx::query("insert into content_document_head (document_id) values ($1)")
        .bind(document_id)
        .execute(pool)
        .await?;
    let identity =
        admitted.then(|| ironrag_contracts::documents::RevisionAdmissionIdentity::Mutation {
            source_identity: None,
            materialization_token: None,
            visibility_intent_id: None,
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                checksum: checksum.clone(),
                byte_size: 14,
                mime_type: "text/plain".to_owned(),
                file_name: "safe.txt".to_owned(),
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        });
    sqlx::query("insert into content_revision (id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,admission_identity) values ($1,$2,$3,$4,1,'upload',$5,'text/plain',14,$6)")
        .bind(revision_id).bind(document_id).bind(fixture.workspace_id).bind(fixture.library_id).bind(&checksum)
        .bind(identity.map(sqlx::types::Json)).execute(pool).await?;
    Ok((document_id, revision_id, checksum))
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn orphan_create_refuses_owned_or_ambiguous_state_without_writes() -> Result<()> {
    use ironrag_backend::services::content::service::UploadInlineDocumentCommand;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool=&fixture.state.persistence.postgres;
        for mode in ["admission-identity", "head", "storage", "source-identity", "mutation", "history", "recovery", "descriptor", "job-attempt", "visibility"] {
            let key=format!("unsafe-{mode}");
            let (document_id,revision_id,_)=seed_upload_without_head(&fixture,&key,mode == "admission-identity").await?;
            match mode {
                "admission-identity" => {}
                "head" => { sqlx::query("update content_document_head set active_revision_id=$2 where document_id=$1").bind(document_id).bind(revision_id).execute(pool).await?; }
                "storage" => { sqlx::query("update content_revision set storage_key='historical/source' where id=$1").bind(revision_id).execute(pool).await?; }
                "source-identity" => { sqlx::query("update content_revision set source_uri='upload://different-source.txt' where id=$1").bind(revision_id).execute(pool).await?; }
                "mutation" => {
                    content_lifecycle_support::seed_pending_content_mutation(pool,content_lifecycle_support::PendingContentMutationSeed {
                        workspace_id:fixture.workspace_id,library_id:fixture.library_id,operation_kind:"replace",request_surface:"rest",mutation_state:"accepted",
                        request_identity:&serde_json::json!({"target":{"kind":"existing","documentId":document_id}}),
                    }).await?;
                }
                "history" => { sqlx::query("insert into content_revision (document_id,workspace_id,library_id,revision_number,parent_revision_id,content_source_kind,checksum,mime_type,byte_size) values ($1,$2,$3,2,$4,'upload','sha256:history','text/plain',1)").bind(document_id).bind(fixture.workspace_id).bind(fixture.library_id).bind(revision_id).execute(pool).await?; }
                "recovery" => { sqlx::query("update content_document_head set recovery_attempts_count=1 where document_id=$1").bind(document_id).execute(pool).await?; }
                "descriptor" => { ironrag_backend::infra::repositories::content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(pool,document_id).await?; }
                "job-attempt" => {
                    let job_id:Uuid=sqlx::query_scalar("insert into ingest_job (workspace_id,library_id,knowledge_document_id,knowledge_revision_id,job_kind) values ($1,$2,$3,$4,'reindex') returning id")
                        .bind(fixture.workspace_id).bind(fixture.library_id).bind(document_id).bind(revision_id).fetch_one(pool).await?;
                    sqlx::query("insert into ingest_attempt (job_id,attempt_number,attempt_state) values ($1,1,'running')").bind(job_id).execute(pool).await?;
                }
                "visibility" => {
                    let request=ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
                        workspace_id:fixture.workspace_id,library_id:fixture.library_id,document_id,
                        requested_by_principal_id:None,request_surface:"rest".to_owned(),
                        intent:ironrag_contracts::documents::CreateVisibilityIntentRequest {
                            idempotency_key:format!("intent-{key}"),source_identity:"neutral-source".to_owned(),materialization_token:None,
                            replacement:content_request(&fixture,"unused").revision.context("replacement")?.replacement,
                        },
                    };
                    ironrag_backend::infra::repositories::admission_repository::admit_visibility_intent_with_failpoint(pool,&request,None).await?;
                }
                _ => unreachable!("test scenario"),
            }
            let before=admission_row_counts(&fixture).await?;
            let before_document:serde_json::Value=sqlx::query_scalar("select to_jsonb(d) from content_document d where id=$1").bind(document_id).fetch_one(pool).await?;
            let before_head:serde_json::Value=sqlx::query_scalar("select to_jsonb(h) from content_document_head h where document_id=$1").bind(document_id).fetch_one(pool).await?;
            let before_revisions:serde_json::Value=sqlx::query_scalar("select jsonb_agg(to_jsonb(r) order by revision_number) from content_revision r where document_id=$1").bind(document_id).fetch_one(pool).await?;
            let error=fixture.state.application_services.content.upload_inline_document(&fixture.state,UploadInlineDocumentCommand {
                workspace_id:fixture.workspace_id,library_id:fixture.library_id,external_key:Some(key.clone()),
                idempotency_key:Some(key),requested_by_principal_id:None,request_surface:"rest".to_owned(),source_identity:None,
                title:None,document_hint:None,file_name:"safe.txt".to_owned(),mime_type:Some("text/plain".to_owned()),
                file_bytes:b"neutral source".to_vec(),parent_external_key:None,
            }).await.expect_err("unsafe state must not be adopted");
            assert!(matches!(error,ApiError::DocumentAdmissionIncomplete(_)),"{mode}: {error:?}");
            assert_eq!(before,admission_row_counts(&fixture).await?,"{mode}");
            let after_document:serde_json::Value=sqlx::query_scalar("select to_jsonb(d) from content_document d where id=$1").bind(document_id).fetch_one(pool).await?;
            assert_eq!(before_document,after_document,"{mode}");
            let after_head:serde_json::Value=sqlx::query_scalar("select to_jsonb(h) from content_document_head h where document_id=$1").bind(document_id).fetch_one(pool).await?;
            let after_revisions:serde_json::Value=sqlx::query_scalar("select jsonb_agg(to_jsonb(r) order by revision_number) from content_revision r where document_id=$1").bind(document_id).fetch_one(pool).await?;
            assert_eq!(before_head,after_head,"{mode}");
            assert_eq!(before_revisions,after_revisions,"{mode}");
        }
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn missing_descriptor_replacement_materializes_atomically_at_all_failpoints() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let (document_id, revision_id, _) =
            seed_upload_without_head(&fixture, "explicit-replacement", false).await?;
        let mut request = content_request(&fixture, "explicit-replacement-key");
        request.target = ContentAdmissionTarget::Existing { document_id };
        request.operation_kind = "replace".to_owned();
        let before = admission_row_counts(&fixture).await?;
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
            let error = admit_content_with_failpoint(
                &fixture.state.persistence.postgres,
                &request,
                Some(failpoint),
            )
            .await
            .expect_err("injected rollback");
            assert!(matches!(error,AdmissionError::InjectedFailure(actual) if actual==failpoint));
            assert_eq!(before, admission_row_counts(&fixture).await?, "{failpoint:?}");
            let descriptors: i64 =
                sqlx::query_scalar("select count(*) from knowledge_document where document_id=$1")
                    .bind(document_id)
                    .fetch_one(&fixture.state.persistence.postgres)
                    .await?;
            assert_eq!(
                descriptors, 0,
                "descriptor rematerialization must roll back: {failpoint:?}"
            );
        }
        let admitted =
            admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
                .await?
                .into_bundle();
        assert_eq!(admitted.document.id, document_id);
        assert_eq!(
            admitted.revision.context("source revision")?.parent_revision_id,
            Some(revision_id)
        );
        let descriptors: i64 =
            sqlx::query_scalar("select count(*) from knowledge_document where document_id=$1")
                .bind(document_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        assert_eq!(descriptors, 1);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn orphan_create_http_completes_without_replacing_history() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        let (document_id,revision_id,checksum) = seed_upload_without_head(&fixture,"orphan-neutral",false).await?;
        let original: serde_json::Value = sqlx::query_scalar("select to_jsonb(r) from content_revision r where id=$1")
            .bind(revision_id).fetch_one(pool).await?;
        let original_document:serde_json::Value=sqlx::query_scalar("select to_jsonb(d)-'revision_sequence' from content_document d where id=$1")
            .bind(document_id).fetch_one(pool).await?;
        // Cancellation after persistence must reclaim the staged blob without
        // acquiring ownership of, or changing, the historical orphan.
        let staged_key=ironrag_backend::services::content::storage::ContentStorageService::build_revision_storage_key(
            fixture.workspace_id,fixture.library_id,"safe.txt",&checksum);
        let probe=ironrag_backend::shared::cancellation::one_shot_pause_probe::install();
        let caller=tokio::spawn({
            let state=fixture.state.clone();
            let command=ironrag_backend::services::content::service::UploadInlineDocumentCommand {
                workspace_id:fixture.workspace_id,library_id:fixture.library_id,
                external_key:Some("orphan-neutral".to_owned()),idempotency_key:Some("aborted-orphan".to_owned()),
                requested_by_principal_id:None,request_surface:"rest".to_owned(),source_identity:None,
                title:None,document_hint:None,file_name:"safe.txt".to_owned(),mime_type:Some("text/plain".to_owned()),
                file_bytes:b"neutral source".to_vec(),parent_external_key:None,
            };
            async move { state.application_services.content.upload_inline_document(&state,command).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10),probe.reached).await??;
        assert!(fixture.state.content_storage.has_revision_source(&staged_key).await?);
        caller.abort();
        assert!(caller.await.expect_err("aborted caller").is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(10),async {
            let mut interval=tokio::time::interval(std::time::Duration::from_millis(20));
            while fixture.state.content_storage.has_revision_source(&staged_key).await? { interval.tick().await; }
            Ok::<(),anyhow::Error>(())
        }).await??;
        let after_abort:serde_json::Value=sqlx::query_scalar("select to_jsonb(r) from content_revision r where id=$1")
            .bind(revision_id).fetch_one(pool).await?;
        assert_eq!(original,after_abort);
        let token = crate::iam_token_support::mint_api_token(pool,Some(fixture.workspace_id),"workspace","orphan-http",&["documents:write","projects:write"]).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let path = format!("/content/libraries/{}/documents",fixture.library_id);
        let body = "--boundary\r\nContent-Disposition: form-data; name=\"external_key\"\r\n\r\norphan-neutral\r\n--boundary\r\nContent-Disposition: form-data; name=\"idempotency_key\"\r\n\r\norphan-retry\r\n--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"safe.txt\"\r\nContent-Type: text/plain\r\n\r\nneutral source\r\n--boundary--\r\n";
        let request_for = |body:String| Request::post(&path).header("content-type","multipart/form-data; boundary=boundary")
            .header("authorization",format!("Bearer {}",token.plaintext)).body(Body::from(body)).expect("request");
        let before_rejection=admission_row_counts(&fixture).await?;
        for changed in [body.replace("neutral source","different source"),body.replace("neutral source","neutral sourcE"),body.replace("text/plain","text/markdown")] {
            let response=router.clone().oneshot(request_for(changed)).await?;
            assert_eq!(response.status(),StatusCode::CONFLICT);
            let error:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await?)?;
            assert_eq!(error["code"],"document_admission_incomplete");
        }
        let spoof=serde_json::json!({"externalKey":"orphan-neutral","fileName":"safe.txt","idempotencyKey":"orphan-retry","contentSourceKind":"upload","checksum":checksum,"mimeType":"text/plain","byteSize":14,"storageKey":"claimed/source","sourceUri":"upload://safe.txt","title":"safe.txt"});
        let response=router.clone().oneshot(Request::post(&path).header("content-type","application/json")
            .header("authorization",format!("Bearer {}",token.plaintext)).body(Body::from(spoof.to_string()))?).await?;
        assert_eq!(response.status(),StatusCode::CONFLICT);
        let error:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await?)?;
        assert_eq!(error["code"],"document_admission_incomplete");
        let mut anonymous=request_for(body.to_owned());
        anonymous.headers_mut().remove("authorization");
        assert_eq!(router.clone().oneshot(anonymous).await?.status(),StatusCode::UNAUTHORIZED);
        let foreign_workspace=ironrag_backend::infra::repositories::catalog_repository::create_workspace(pool,"orphan-foreign","Foreign",None).await?;
        let foreign=crate::iam_token_support::mint_api_token(pool,Some(foreign_workspace.id),"workspace","orphan-foreign",&["documents:write","projects:write"]).await?;
        let mut wrong_scope=request_for(body.to_owned());
        wrong_scope.headers_mut().insert("authorization",format!("Bearer {}",foreign.plaintext).parse()?);
        assert_eq!(router.clone().oneshot(wrong_scope).await?.status(),StatusCode::UNAUTHORIZED);
        assert_eq!(admission_row_counts(&fixture).await?,before_rejection);
        let child_id:Uuid=sqlx::query_scalar("insert into content_document (workspace_id,library_id,external_key,parent_document_id) values ($1,$2,'dependent-child',$3) returning id")
            .bind(fixture.workspace_id).bind(fixture.library_id).bind(document_id).fetch_one(pool).await?;
        let response=router.clone().oneshot(request_for(body.to_owned())).await?;
        assert_eq!(response.status(),StatusCode::CONFLICT,"dependencies must not be adopted");
        sqlx::query("update content_document set parent_document_id=null where id=$1").bind(child_id).execute(pool).await?;
        let reserved=body.replace("orphan-neutral","reserved-owner").replace("orphan-retry","reserved-key").replace("neutral source","different source");
        assert_eq!(router.clone().oneshot(request_for(reserved)).await?.status(),StatusCode::CREATED);
        let response=router.clone().oneshot(request_for(body.replace("orphan-retry","reserved-key"))).await?;
        assert_eq!(response.status(),StatusCode::CONFLICT);
        let error:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await?)?;
        assert_eq!(error["code"],"idempotency_conflict","an occupied request key must be checked before target completion");
        let (response,repeated)=tokio::join!(router.clone().oneshot(request_for(body.to_owned())),router.clone().oneshot(request_for(body.to_owned())));
        let response=response?;
        assert_eq!(response.status(),StatusCode::CREATED,"identical staged bytes must complete the canonical orphan");
        let first:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),128*1024).await?)?;
        let response=repeated?;
        assert_eq!(response.status(),StatusCode::CREATED);
        let replay:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),128*1024).await?)?;
        assert_eq!(first["document"]["document"]["id"],serde_json::json!(document_id));
        assert!(first["mutation"]["mutation"]["id"].is_string());
        assert_eq!(first["mutation"]["mutation"]["id"],replay["mutation"]["mutation"]["id"]);
        let unchanged:serde_json::Value=sqlx::query_scalar("select to_jsonb(r) from content_revision r where id=$1")
            .bind(revision_id).fetch_one(pool).await?;
        assert_eq!(original,unchanged);
        let unchanged_document:serde_json::Value=sqlx::query_scalar("select to_jsonb(d)-'revision_sequence' from content_document d where id=$1")
            .bind(document_id).fetch_one(pool).await?;
        assert_eq!(original_document,unchanged_document,"canonical identity and creation metadata must survive completion");
        let (storage_key,new_revision_number,parent_revision_id):(String,i32,Option<Uuid>)=sqlx::query_as("select storage_key,revision_number,parent_revision_id from content_revision where document_id=$1 and id<>$2")
            .bind(document_id).bind(revision_id).fetch_one(pool).await?;
        assert_eq!(new_revision_number,2);
        assert_eq!(parent_revision_id,Some(revision_id));
        let revision_admission: serde_json::Value = sqlx::query_scalar(
            "select admission_identity from content_revision where document_id=$1 and id<>$2",
        )
        .bind(document_id)
        .bind(revision_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(revision_admission["origin"], "mutation");
        assert_eq!(revision_admission["sourceIdentity"], serde_json::Value::Null);
        assert_eq!(revision_admission["materializationToken"], serde_json::Value::Null);
        assert_eq!(revision_admission["visibilityIntentId"], serde_json::Value::Null);
        assert_eq!(revision_admission["replacement"]["checksum"], checksum);
        assert_eq!(revision_admission["replacement"]["byteSize"], 14);
        assert_eq!(revision_admission["replacement"]["mimeType"], "text/plain");
        assert_eq!(revision_admission["replacement"]["fileName"], "safe.txt");
        assert_eq!(fixture.state.content_storage.read_revision_source(&storage_key).await?,b"neutral source");
        let graph:(i64,i64,i64,i64)=sqlx::query_as("select (select count(*) from content_revision where document_id=$1),(select count(*) from content_mutation_item where document_id=$1),(select count(*) from knowledge_revision where document_id=$1),(select count(*) from ingest_job where knowledge_document_id=$1)")
            .bind(document_id).fetch_one(pool).await?;
        assert_eq!(graph,(2,1,1,1));
        let identity:serde_json::Value=sqlx::query_scalar("select m.request_identity from content_mutation m join content_mutation_item i on i.mutation_id=m.id where i.document_id=$1")
            .bind(document_id).fetch_one(pool).await?;
        assert_eq!(identity["target"]["kind"],"new","completion must not rewrite the frozen CREATE into a replacement");
        assert_eq!(identity["target"]["externalKey"],"orphan-neutral");
        let unpublished:bool=sqlx::query_scalar("select active_revision_id is null and readable_revision_id is null and latest_mutation_id is not null from content_document_head where document_id=$1")
            .bind(document_id).fetch_one(pool).await?;
        assert!(unpublished,"admission must not publish source before a successful worker");
        let response=router.clone().oneshot(request_for(body.replace("orphan-retry","fresh-key-same-target"))).await?;
        assert_eq!(response.status(),StatusCode::CONFLICT);
        let error:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await?)?;
        assert_eq!(error["code"],"duplicate_content","a fully admitted external-key duplicate retains its existing contract");
        let response=router.oneshot(request_for(body.replace("orphan-neutral","other-key").replace("orphan-retry","another-request"))).await?;
        assert_eq!(response.status(),StatusCode::CONFLICT);
        let error:serde_json::Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await?)?;
        assert_eq!(error["code"],"duplicate_content","a fully admitted duplicate retains its existing contract");
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_http_intent_is_strict_authorized_and_replayable() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document = fixture.state.application_services.content.create_document(&fixture.state,CreateDocumentCommand {
            workspace_id:fixture.workspace_id,library_id:fixture.library_id,
            external_key:Some("visibility-http".to_owned()),file_name:Some("safe.txt".to_owned()),
            created_by_principal_id:None,parent_external_key:None,
        }).await?;
        let token = crate::iam_token_support::mint_api_token(&fixture.state.persistence.postgres,
            Some(fixture.workspace_id),"workspace","visibility-http",&["documents:write"]).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let path = format!("/content/documents/{}/visibility-intents",document.id);
        let payload = serde_json::json!({
            "idempotencyKey":"observed-change", "sourceIdentity":"source-opaque", "materializationToken":null,
            "replacement": {"checksum":format!("sha256:{}","a".repeat(64)),"byteSize":0,
                "mimeType":" TEXT/PLAIN ","fileName":" safe.txt ","title":null,
                "languageCode":null,"sourceUri":null,"documentHint":null},
        });
        let request = |body:serde_json::Value| Request::post(&path)
            .header("content-type","application/json")
            .header("authorization",format!("Bearer {}",token.plaintext))
            .body(Body::from(body.to_string())).expect("test request");
        let created = router.clone().oneshot(request(payload.clone())).await?;
        assert_eq!(created.status(),StatusCode::CREATED);
        let receipt:ironrag_contracts::documents::VisibilityIntentReceipt = serde_json::from_slice(
            &axum::body::to_bytes(created.into_body(),64*1024).await?)?;
        assert_eq!(receipt.visibility_intent_id,receipt.mutation_item_id);
        assert_eq!(receipt.revision_floor,0);
        assert_eq!(receipt.replacement.mime_type,"text/plain");
        let replay = router.clone().oneshot(request(payload.clone())).await?;
        assert_eq!(replay.status(),StatusCode::OK);
        let original:ironrag_contracts::documents::VisibilityIntentReceipt = serde_json::from_slice(
            &axum::body::to_bytes(replay.into_body(),64*1024).await?)?;
        assert_eq!(receipt,original);
        let jobs:i64 = sqlx::query_scalar("select count(*) from ingest_job where mutation_id=$1")
            .bind(receipt.mutation_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(jobs,0);
        let before_invalid=admission_row_counts(&fixture).await?;
        for field in ["sourceIdentity","idempotencyKey"] {
            let mut invalid=payload.clone();
            invalid[field]=serde_json::json!(" ");
            assert_eq!(router.clone().oneshot(request(invalid)).await?.status(),StatusCode::BAD_REQUEST,"blank {field}");
        }
        let other_workspace=ironrag_backend::infra::repositories::catalog_repository::create_workspace(
            &fixture.state.persistence.postgres,"visibility-other","Visibility other",None,
        ).await?;
        let foreign=crate::iam_token_support::mint_api_token(&fixture.state.persistence.postgres,
            Some(other_workspace.id),"workspace","visibility-other",&["documents:write"]).await?;
        let mut foreign_request=request(payload.clone());
        foreign_request.headers_mut().insert("authorization",format!("Bearer {}",foreign.plaintext).parse()?);
        assert_eq!(router.clone().oneshot(foreign_request).await?.status(),StatusCode::UNAUTHORIZED);
        for field in ["materializationToken","sourceIdentity","idempotencyKey"] {
            let mut invalid=payload.clone();
            invalid.as_object_mut().context("object")?.remove(field);
            let response=router.clone().oneshot(request(invalid)).await?;
            assert_eq!(response.status(),StatusCode::UNPROCESSABLE_ENTITY,"missing {field}");
        }
        let mut removed_policy=payload.clone();
        removed_policy["visibilityPolicy"]=serde_json::json!("suppressed_until_ready");
        assert_eq!(router.clone().oneshot(request(removed_policy)).await?.status(),StatusCode::UNPROCESSABLE_ENTITY);
        let mut conflicting=payload.clone();
        conflicting["replacement"]["title"]=serde_json::json!("changed metadata");
        assert_eq!(router.clone().oneshot(request(conflicting)).await?.status(),StatusCode::CONFLICT);
        let denied=crate::iam_token_support::mint_api_token(&fixture.state.persistence.postgres,
            Some(fixture.workspace_id),"workspace","visibility-read-only",&["documents:read"]).await?;
        let mut denied_request=request(payload.clone());
        denied_request.headers_mut().insert("authorization",format!("Bearer {}",denied.plaintext).parse()?);
        // The canonical permission guard currently reports Unauthorized for an
        // authenticated caller without the required write grant.
        assert_eq!(router.clone().oneshot(denied_request).await?.status(),StatusCode::UNAUTHORIZED);
        let mut anonymous=request(payload.clone());
        anonymous.headers_mut().remove("authorization");
        assert_eq!(router.clone().oneshot(anonymous).await?.status(),StatusCode::UNAUTHORIZED);
        assert_eq!(admission_row_counts(&fixture).await?,before_invalid,"rejected boundaries must not admit any bundle");
        sqlx::query("update content_document set document_state='deleted',deleted_at=now() where id=$1")
            .bind(document.id).execute(&fixture.state.persistence.postgres).await?;
        assert_eq!(router.oneshot(request(payload)).await?.status(),StatusCode::BAD_REQUEST);
        let version=ironrag_backend::interfaces::http::health::version(axum::extract::State(fixture.state.clone())).await;
        assert_eq!(version.0.content_visibility_contract,"visibility-intent/1");
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

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
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                file_name: "atomic.txt".to_owned(),
                checksum: format!(
                    "sha256:{}",
                    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(key.as_bytes()))
                ),
                mime_type: "text/plain".to_string(),
                byte_size: 64,
                title: Some("Atomic admission".to_string()),
                language_code: Some("en".to_string()),
                source_uri: Some(format!("upload://{key}")),
                document_hint: None,
            },
            visibility_intent_id: None,
            base_revision_id: None,
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

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_payload_replay_preserves_original_artifact() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let request = content_request(&fixture, "stable-payload");
        let first =
            admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
                .await?
                .into_bundle();
        let counts = admission_row_counts(&fixture).await?;
        let mut retry = request.clone();
        retry.revision.as_mut().context("revision")?.storage_key =
            Some("different/staged/artifact".to_owned());
        let repeated =
            admit_content_with_failpoint(&fixture.state.persistence.postgres, &retry, None).await?;
        assert!(matches!(repeated, AdmissionOutcome::Replayed(_)));
        let repeated = repeated.into_bundle();
        assert_eq!(repeated.mutation.id, first.mutation.id);
        assert_eq!(
            repeated.revision.as_ref().map(|row| row.id),
            first.revision.as_ref().map(|row| row.id)
        );
        assert_eq!(
            repeated.revision.as_ref().and_then(|row| row.storage_key.as_ref()),
            first.revision.as_ref().and_then(|row| row.storage_key.as_ref()),
            "replay retains committed artifact ownership"
        );
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        retry.revision.as_mut().context("revision")?.replacement.title =
            Some("changed explicit metadata".to_owned());
        let error = admit_content_with_failpoint(&fixture.state.persistence.postgres, &retry, None)
            .await
            .expect_err("changed metadata conflicts despite identical bytes");
        assert!(matches!(error, AdmissionError::IdempotencyConflict { .. }));
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_bound_payload_requires_current_exact_intent() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let initial = admit_content_with_failpoint(postgres, &content_request(&fixture, "binding-base"), None).await?.into_bundle();
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1")
            .bind(initial.mutation.id).execute(postgres).await?;
        let mut payload = content_request(&fixture, "bound-safe");
        payload.operation_kind = "replace".to_owned();
        payload.source_identity = None;
        payload.target = ContentAdmissionTarget::Existing { document_id: initial.document.id };
        let intent = ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id,
            document_id: initial.document.id, requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: "binding-denial".to_owned(), source_identity: "bound-source".to_owned(),
                materialization_token: Some("opaque-token".to_owned()),
                replacement: payload.revision.as_ref().context("replacement")?.replacement.clone(),
            },
        };
        let accepted = ironrag_backend::infra::repositories::admission_repository::admit_visibility_intent_with_failpoint(postgres, &intent, None).await?.into_bundle();
        let before = admission_row_counts(&fixture).await?;
        let unbound = admit_content_with_failpoint(postgres, &payload, None).await;
        assert!(matches!(unbound, Err(AdmissionError::VisibilityIntentRequired { .. })), "protected document admitted an unbound payload");
        assert_eq!(admission_row_counts(&fixture).await?, before);
        let mut metadata = payload.clone();
        metadata.revision = None;
        assert!(matches!(admit_content_with_failpoint(postgres, &metadata, None).await,
            Err(AdmissionError::VisibilityIntentRequired { .. })));
        assert_eq!(admission_row_counts(&fixture).await?, before);
        payload.revision.as_mut().context("replacement")?.visibility_intent_id = Some(accepted.receipt.visibility_intent_id);
        for field in ["filename", "mime", "title", "language", "uri", "hint", "checksum", "size", "source"] {
            let mut mismatched = payload.clone();
            let descriptor = &mut mismatched.revision.as_mut().context("replacement")?.replacement;
            match field {
                "filename" => descriptor.file_name = "other.txt".to_owned(),
                "mime" => descriptor.mime_type = "text/markdown".to_owned(),
                "title" => descriptor.title = None,
                "language" => descriptor.language_code = None,
                "uri" => descriptor.source_uri = None,
                "hint" => descriptor.document_hint = Some("not accepted".to_owned()),
                "checksum" => descriptor.checksum = format!("sha256:{}", "b".repeat(64)),
                "size" => descriptor.byte_size += 1,
                "source" => mismatched.source_identity = Some("other source".to_owned()),
                _ => unreachable!(),
            }
            assert!(matches!(admit_content_with_failpoint(postgres, &mismatched, None).await,
                Err(AdmissionError::VisibilityBindingConflict { .. })), "{field}");
            assert_eq!(admission_row_counts(&fixture).await?, before);
        }
        payload.revision.as_mut().context("replacement")?.replacement.mime_type = " TEXT/PLAIN ".to_owned();
        let created = admit_content_with_failpoint(postgres, &payload, None).await?.into_bundle();
        let bound: Option<Uuid> = sqlx::query_scalar("select visibility_intent_id from content_mutation_item where id=$1")
            .bind(created.item.as_ref().context("item")?.id).fetch_one(postgres).await?;
        assert_eq!(bound, Some(accepted.receipt.visibility_intent_id));
        let after_created = admission_row_counts(&fixture).await?;
        let mut foreign_replay = payload.clone();
        foreign_replay.source_identity = Some("different source under same accepted key".to_owned());
        assert!(matches!(admit_content_with_failpoint(postgres, &foreign_replay, None).await,
            Err(AdmissionError::VisibilityBindingConflict { .. })));
        assert_eq!(admission_row_counts(&fixture).await?, after_created);
        let snapshot = ironrag_backend::infra::repositories::content_revision_repository::get_revision_replacement_identity(
            postgres, created.revision.as_ref().context("revision")?.id).await?.context("immutable snapshot")?;
        assert_eq!(snapshot, accepted.receipt.replacement);
        assert_eq!(created.revision.as_ref().context("revision")?.title, snapshot.title);
        sqlx::query("update content_document set source_file_name='changed-document-label.txt' where id=$1")
            .bind(initial.document.id).execute(postgres).await?;
        assert_eq!(ironrag_backend::infra::repositories::content_revision_repository::get_revision_replacement_identity(
            postgres, created.revision.as_ref().context("revision")?.id).await?, Some(snapshot));
        let mut newer = intent.clone();
        newer.intent.idempotency_key = "newer-binding-denial".to_owned();
        let next = ironrag_backend::infra::repositories::admission_repository::admit_visibility_intent_with_failpoint(postgres, &newer, None).await?.into_bundle();
        let original = admit_content_with_failpoint(postgres, &payload, None).await?;
        assert!(matches!(original, AdmissionOutcome::Replayed(_)));
        assert_eq!(original.into_bundle().mutation.id, created.mutation.id);
        let head_intent: Option<Uuid> = sqlx::query_scalar("select visibility_intent_id from content_document_head where document_id=$1")
            .bind(initial.document.id).fetch_one(postgres).await?;
        assert_eq!(head_intent, Some(next.receipt.visibility_intent_id));
        payload.idempotency_key = Some("stale-binding-new-key".to_owned());
        let stale = admit_content_with_failpoint(postgres, &payload, None).await;
        assert!(matches!(stale, Err(AdmissionError::VisibilityIntentSuperseded { .. })));
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_append_explicit_base_replay_and_boundaries() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use ironrag_backend::services::content::service::AppendInlineMutationCommand;
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let mut base_request = content_request(&fixture, "append-base");
        let descriptor = &mut base_request.revision.as_mut().context("base descriptor")?.replacement;
        descriptor.checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"base")));
        descriptor.byte_size = 4;
        let key = fixture.state.content_storage.persist_revision_source(fixture.workspace_id, fixture.library_id,
            &descriptor.file_name, &descriptor.checksum, b"base").await?;
        base_request.revision.as_mut().context("base")?.storage_key = Some(key);
        let base = admit_content_with_failpoint(pg, &base_request, None).await?.into_bundle();
        let document_id = base.document.id;
        let base_id = base.revision.as_ref().context("base revision")?.id;
        let item_id = base.item.as_ref().context("base item")?.id;
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1").bind(base.mutation.id).execute(pg).await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(item_id).execute(pg).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now() where revision_id=$1").bind(base_id).execute(pg).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(base_id).execute(pg).await?;
        let mut append_request = base_request.clone();
        append_request.target = ContentAdmissionTarget::Existing { document_id };
        append_request.operation_kind = "append".to_owned();
        append_request.idempotency_key = Some("append-replay".to_owned());
        let revision = append_request.revision.as_mut().context("append descriptor")?;
        revision.content_source_kind = "append".to_owned();
        revision.base_revision_id = Some(base_id);
        revision.replacement.checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"base\n\ntail")));
        revision.replacement.byte_size = 10;
        revision.storage_key = Some(fixture.state.content_storage.persist_revision_source(fixture.workspace_id, fixture.library_id,
            &revision.replacement.file_name, &revision.replacement.checksum,b"base\n\ntail").await?);
        let appended = admit_content_with_failpoint(pg, &append_request, None).await?.into_bundle();
        let appended_id = appended.revision.as_ref().context("appended revision")?.id;
        let appended_item = appended.item.as_ref().context("appended item")?.id;
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1").bind(appended.mutation.id).execute(pg).await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(appended_item).execute(pg).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now() where revision_id=$1").bind(appended_id).execute(pg).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(appended_id).execute(pg).await?;
        // Mutable labels are not append source metadata.
        sqlx::query("update content_document set source_file_name='mutable-label.txt' where id=$1").bind(document_id).execute(pg).await?;
        let command = AppendInlineMutationCommand {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id,
            base_revision_id: base_id, idempotency_key: Some("append-replay".to_owned()),
            requested_by_principal_id: None, request_surface: "rest".to_owned(),
            source_identity: base_request.source_identity.clone(), appended_text: "tail".to_owned(),
        };
        let before = admission_row_counts(&fixture).await?;
        let service = &fixture.state.application_services.content;
        let replay = service.append_inline_mutation(&fixture.state,command.clone()).await?;
        assert!(replay.replayed);
        assert_eq!(replay.items[0].result_revision_id,Some(appended_id));
        let stored = ironrag_backend::infra::repositories::content_revision_repository::get_revision_by_id(pg,appended_id).await?.context("stored append")?;
        assert_eq!(stored.storage_key,appended.revision.as_ref().context("original append")?.storage_key);
        assert_eq!(stored.parent_revision_id,Some(base_id));
        let attempts: i64 = sqlx::query_scalar("select count(*) from ingest_attempt a join ingest_job j on j.id=a.job_id where j.mutation_id=$1")
            .bind(appended.mutation.id).fetch_one(pg).await?;
        assert_eq!(attempts,0,"exact terminal replay must not rematerialize");
        assert_eq!(admission_row_counts(&fixture).await?, before);
        let head = content_document_head_repository::get_document_head(pg,document_id).await?.context("head")?;
        assert_eq!(head.readable_revision_id,Some(appended_id));
        for changed in ["text", "base", "old", "scope", "missing"] {
            let mut invalid = command.clone();
            match changed {
                "text" => invalid.appended_text = "different".to_owned(),
                "base" => invalid.base_revision_id = appended_id,
                "old" => invalid.idempotency_key = Some("new-old-base".to_owned()),
                "scope" => invalid.library_id = Uuid::now_v7(),
                "missing" => invalid.base_revision_id = Uuid::now_v7(),
                _ => unreachable!(),
            }
            let error = service.append_inline_mutation(&fixture.state, invalid).await.expect_err("invalid append refused");
            use ironrag_backend::interfaces::http::router_support::ApiError;
            match changed {
                "text" | "base" => assert!(matches!(error,ApiError::IdempotencyConflict(_)),"{changed}: {error:?}"),
                "old" => assert!(matches!(error,ApiError::VisibilityBindingConflict(_)),"{error:?}"),
                "scope" => assert!(matches!(error,ApiError::Conflict(_)),"{error:?}"),
                "missing" => assert!(matches!(error,ApiError::NotFound(_)),"{error:?}"),
                _ => unreachable!(),
            }
            assert_eq!(admission_row_counts(&fixture).await?,before,"{changed}");
        }
        let token = crate::iam_token_support::mint_api_token(pg,Some(fixture.workspace_id),"workspace","append-http",&["documents:write"]).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let path = format!("/content/documents/{document_id}/revisions");
        let body = serde_json::json!({"mode":"append","appendedText":"tail","baseRevisionId":base_id});
        for field in ["fileName","mimeType","title","languageCode","sourceUri","documentHint","markdown","visibilityIntentId"] {
            for value in [serde_json::Value::Null, serde_json::json!("explicit")] {
                let mut invalid = body.clone(); invalid[field] = value;
                let response = router.clone().oneshot(Request::post(&path).header("content-type","application/json")
                    .header("authorization",format!("Bearer {}",token.plaintext)).body(Body::from(invalid.to_string()))?).await?;
                assert_eq!(response.status(),StatusCode::BAD_REQUEST,"{field}");
            }
        }
        let mut no_base = body.clone(); no_base.as_object_mut().context("body")?.remove("baseRevisionId");
        let response = router.oneshot(Request::post(&path).header("content-type","application/json")
            .header("authorization",format!("Bearer {}",token.plaintext)).body(Body::from(no_base.to_string()))?).await?;
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
        let intent = ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id,document_id,requested_by_principal_id:None,request_surface:"rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest { idempotency_key:"append-deny".to_owned(),source_identity:"source".to_owned(),materialization_token:None,
                replacement:append_request.revision.as_ref().context("replacement")?.replacement.clone() },
        };
        ironrag_backend::infra::repositories::admission_repository::admit_visibility_intent_with_failpoint(pg,&intent,None).await?;
        let mut protected = command; protected.base_revision_id = appended_id; protected.idempotency_key = Some("protected-append".to_owned());
        assert!(matches!(service.append_inline_mutation(&fixture.state,protected).await,Err(ironrag_backend::interfaces::http::router_support::ApiError::VisibilityIntentRequired(_))));
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_mcp_append_dispatcher_replays_busy_and_denied_heads() -> Result<()> {
    use ironrag_backend::{
        domains::iam::PrincipalKind,
        interfaces::http::auth::{AuthContext, AuthGrant, AuthTokenKind},
        mcp_types::McpUpdateDocumentRequest,
    };
    use sha2::{Digest, Sha256};
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let principal = iam_repository::create_principal(pg, "api_token", "MCP append replay", None).await?;
        let auth = AuthContext {
            token_id: Uuid::now_v7(), principal_id: principal.id, parent_principal_id: None,
            workspace_id: Some(fixture.workspace_id), token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: vec!["document_write".to_owned(), "library_write".to_owned(), "operation_read".to_owned()],
            grants: ["document_write", "library_write", "operation_read"].into_iter().map(|permission| AuthGrant {
                id: Uuid::now_v7(), resource_kind: "workspace".to_owned(), resource_id: fixture.workspace_id,
                permission_kind: permission.to_owned(), workspace_id: Some(fixture.workspace_id), library_id: None, document_id: None,
            }).collect(), workspace_memberships: vec![], visible_workspace_ids: [fixture.workspace_id].into_iter().collect(),
            is_system_admin: false, system_role: None,
        };
        let library_ref: String = sqlx::query_scalar("select w.slug || '/' || l.slug from catalog_library l join catalog_workspace w on w.id=l.workspace_id where l.id=$1")
            .bind(fixture.library_id).fetch_one(pg).await?;
        let mut request = content_request(&fixture, "mcp-append-base");
        let descriptor = &mut request.revision.as_mut().context("base")?.replacement;
        descriptor.checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"base")));
        descriptor.byte_size = 4;
        let key = fixture.state.content_storage.persist_revision_source(fixture.workspace_id, fixture.library_id,
            &descriptor.file_name, &descriptor.checksum, b"base").await?;
        request.revision.as_mut().context("base")?.storage_key = Some(key);
        let base = admit_content_with_failpoint(pg, &request, None).await?.into_bundle();
        let document_id = base.document.id;
        let base_id = base.revision.as_ref().context("base revision")?.id;
        let base_item = base.item.as_ref().context("base item")?.id;
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1").bind(base.mutation.id).execute(pg).await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(base_item).execute(pg).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now() where revision_id=$1").bind(base_id).execute(pg).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(base_id).execute(pg).await?;
        // Seed the complete MCP-owned bundle, but leave its work accepted. The
        // dispatcher must replay it without requiring a readable/nonbusy head.
        let payload_hash = |id: Uuid| {
            let mut hash = Sha256::new(); hash.update(id.as_bytes()); hash.update(b"tail"); hex::encode(hash.finalize())
        };
        let hash = payload_hash(base_id);
        request.target = ContentAdmissionTarget::Existing { document_id };
        request.operation_kind = "append".to_owned();
        request.request_surface = "mcp".to_owned();
        request.requested_by_principal_id = Some(principal.id);
        request.idempotency_key = Some(format!("mcp:append:{document_id}:{hash}"));
        request.source_identity = Some(hash.clone());
        let revision = request.revision.as_mut().context("append")?;
        revision.content_source_kind = "append".to_owned(); revision.base_revision_id = Some(base_id);
        revision.replacement.checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"base\n\ntail")));
        revision.replacement.byte_size = 10;
        revision.storage_key = Some(fixture.state.content_storage.persist_revision_source(fixture.workspace_id, fixture.library_id,
            &revision.replacement.file_name, &revision.replacement.checksum, b"base\n\ntail").await?);
        let admitted = admit_content_with_failpoint(pg, &request, None).await?.into_bundle();
        let command = |base| McpUpdateDocumentRequest {
            base_revision_id: Some(base), library: library_ref.clone(), document_id,
            operation_kind: ironrag_backend::mcp_types::McpDocumentMutationKind::Append,
            idempotency_key: None, appended_text: Some("tail".to_owned()), replacement_file_name: None,
            replacement_content_base64: None, replacement_mime_type: None,
            title: None, language_code: None, source_uri: None, document_hint: None,
            visibility_intent_id: None,
        };
        let counts = admission_row_counts(&fixture).await?;
        let receipt = call_mcp_append(&auth, &fixture.state, command(base_id)).await?;
        assert_eq!(receipt.operation_id, admitted.async_operation.id);
        assert_eq!(receipt.status, ironrag_backend::mcp_types::McpMutationReceiptStatus::Accepted);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let appended_id = admitted.revision.as_ref().context("append result")?.id;
        let appended_item = admitted.item.as_ref().context("append item")?.id;
        sqlx::query("update content_mutation set mutation_state='applied' where id=$1").bind(admitted.mutation.id).execute(pg).await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(appended_item).execute(pg).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now() where revision_id=$1").bind(appended_id).execute(pg).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(appended_id).execute(pg).await?;
        // Another legitimate identical append uses the new exact base. It has
        // a distinct generated key and replays its own complete accepted bundle.
        let next_hash = payload_hash(appended_id);
        request.idempotency_key = Some(format!("mcp:append:{document_id}:{next_hash}"));
        request.source_identity = Some(next_hash.clone());
        let revision = request.revision.as_mut().context("next append")?;
        revision.base_revision_id = Some(appended_id);
        revision.replacement.checksum = format!("sha256:{}", hex::encode(Sha256::digest(b"base\n\ntail\n\ntail")));
        revision.replacement.byte_size = 16;
        revision.storage_key = Some(fixture.state.content_storage.persist_revision_source(fixture.workspace_id, fixture.library_id,
            &revision.replacement.file_name, &revision.replacement.checksum, b"base\n\ntail\n\ntail").await?);
        let next = admit_content_with_failpoint(pg, &request, None).await?.into_bundle();
        let counts = admission_row_counts(&fixture).await?;
        let receipt = call_mcp_append(&auth, &fixture.state, command(appended_id)).await?;
        assert_eq!(receipt.operation_id, next.async_operation.id);
        assert_ne!(receipt.operation_id, admitted.async_operation.id);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let denial = fixture.state.application_services.content.admit_visibility_intent(&fixture.state,
            ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id,
                requested_by_principal_id: Some(principal.id), request_surface: "mcp".to_owned(),
                intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                    idempotency_key: "mcp-later-denial".to_owned(), source_identity: "new-obligation".to_owned(),
                    materialization_token: None, replacement: request.revision.as_ref().context("next replacement")?.replacement.clone(),
                },
            }).await?;
        let counts = admission_row_counts(&fixture).await?;
        let receipt = call_mcp_append(&auth, &fixture.state, command(base_id)).await?;
        assert_eq!(receipt.operation_id, admitted.async_operation.id);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let head = content_document_head_repository::get_document_head(pg, document_id).await?.context("head")?;
        assert_eq!(head.readable_revision_id, None);
        assert_eq!(head.visibility_intent_id, Some(denial.into_bundle().receipt.visibility_intent_id));
        let attempts: i64 = sqlx::query_scalar("select count(*) from ingest_attempt a join ingest_job j on j.id=a.job_id where j.mutation_id in ($1,$2)")
            .bind(admitted.mutation.id).bind(next.mutation.id).fetch_one(pg).await?;
        assert_eq!(attempts, 0, "MCP replay must never rematerialize");
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_all_http_writers_reject_retired_policy_inputs() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document = fixture.state.application_services.content.create_document(&fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id,
                external_key: Some("retired-policy-inputs".to_owned()), file_name: Some("source.txt".to_owned()),
                created_by_principal_id: None, parent_external_key: None,
            }).await?;
        let token = crate::iam_token_support::mint_api_token(&fixture.state.persistence.postgres,
            Some(fixture.workspace_id), "workspace", "retired-policy-inputs", &["documents:write", "projects:write"]).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let before = admission_row_counts(&fixture).await?;
        let create_path = format!("/content/libraries/{}/documents", fixture.library_id);
        for field in ["visibilityPolicy", "visibility_policy"] {
            for value in [serde_json::Value::Null, serde_json::json!("readable"), serde_json::json!("suppressed_until_ready")] {
                for path in [&create_path, &format!("/content/documents/{}/reprocess", document.id),
                    &format!("/content/documents/{}/revisions", document.id), &format!("/content/documents/{}/visibility-intents", document.id)] {
                    let mut payload = serde_json::json!({"idempotencyKey":"retired-policy-key"});
                    payload[field] = value.clone();
                    let response = router.clone().oneshot(Request::post(path).header("content-type", "application/json")
                        .header("authorization", format!("Bearer {}", token.plaintext))
                        .body(Body::from(payload.to_string()))?).await?;
                    assert!(response.status().is_client_error(), "retired field must not be accepted: {path}/{field}");
                    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
                    assert_ne!(response.status(), StatusCode::FORBIDDEN);
                    let body = axum::body::to_bytes(response.into_body(), 64*1024).await?;
                    let message = std::str::from_utf8(&body)?;
                    assert!(message.contains(field), "must reject the named retired field, not another missing prerequisite: {message}");
                    assert_eq!(admission_row_counts(&fixture).await?, before);
                }
            }
            let multipart = format!("--boundary\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\nreadable\r\n--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"source.txt\"\r\nContent-Type: text/plain\r\n\r\nsafe\r\n--boundary--\r\n");
            for path in [&create_path, &format!("/content/documents/{}/revisions", document.id)] {
                let response = router.clone().oneshot(Request::post(path).header("content-type", "multipart/form-data; boundary=boundary")
                    .header("authorization", format!("Bearer {}", token.plaintext))
                    .body(Body::from(multipart.clone()))?).await?;
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "retired multipart field {field}");
                assert_eq!(admission_row_counts(&fixture).await?, before);
            }
        }
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_http_delete_binds_header_key_to_exact_target() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let mut ids = Vec::new();
        for label in ["first-delete", "second-delete"] {
            ids.push(
                fixture
                    .state
                    .application_services
                    .content
                    .create_document(
                        &fixture.state,
                        CreateDocumentCommand {
                            workspace_id: fixture.workspace_id,
                            library_id: fixture.library_id,
                            external_key: Some(label.to_owned()),
                            file_name: Some(format!("{label}.txt")),
                            created_by_principal_id: None,
                            parent_external_key: None,
                        },
                    )
                    .await?
                    .id,
            );
        }
        let token = crate::iam_token_support::mint_api_token(
            &fixture.state.persistence.postgres,
            Some(fixture.workspace_id),
            "workspace",
            "http-delete-key",
            &["documents:write"],
        )
        .await?;
        let router =
            ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let request = |id, key: &str| {
            Request::delete(format!("/content/documents/{id}"))
                .header("authorization", format!("Bearer {}", token.plaintext))
                .header("Idempotency-Key", key)
                .body(Body::empty())
                .expect("delete request")
        };
        let response = router.clone().oneshot(request(ids[0], "http-delete-exact-key")).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let receipt: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 64 * 1024).await?)?;
        assert_eq!(receipt["mutation"]["idempotency_key"], "http-delete-exact-key");
        let mutation_id: Uuid = serde_json::from_value(receipt["mutation"]["id"].clone())?;
        let counts = admission_row_counts(&fixture).await?;
        let repeated = router.clone().oneshot(request(ids[0], "http-delete-exact-key")).await?;
        assert_eq!(repeated.status(), StatusCode::OK);
        let repeated: serde_json::Value =
            serde_json::from_slice(&axum::body::to_bytes(repeated.into_body(), 64 * 1024).await?)?;
        assert_eq!(repeated["mutation"]["id"], receipt["mutation"]["id"]);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let foreign = router.clone().oneshot(request(ids[1], "http-delete-exact-key")).await?;
        assert_eq!(foreign.status(), StatusCode::CONFLICT);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let second = content_document_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            ids[1],
        )
        .await?
        .context("second target")?;
        assert!(second.deleted_at.is_none());
        let items: Vec<Uuid> = sqlx::query_scalar(
            "select document_id from content_mutation_item where mutation_id=$1",
        )
        .bind(mutation_id)
        .fetch_all(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(items, vec![ids[0]]);
        let blank = router.oneshot(request(ids[1], " ")).await?;
        assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_actorless_upload_replay_reaches_atomic_identity_guard() -> Result<()> {
    use ironrag_backend::services::content::service::UploadInlineDocumentCommand;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let command = UploadInlineDocumentCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            external_key: Some("actorless-upload".to_owned()),
            idempotency_key: Some("actorless-replay-key".to_owned()),
            requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            source_identity: Some("actorless-source".to_owned()),
            file_name: "actorless.txt".to_owned(),
            title: None,
            document_hint: None,
            mime_type: Some("text/plain".to_owned()),
            file_bytes: b"actorless bytes".to_vec(),
            parent_external_key: None,
        };
        let service = &fixture.state.application_services.content;
        let first = service.upload_inline_document(&fixture.state, command.clone()).await?;
        let before = admission_row_counts(&fixture).await?;
        let second = service
            .upload_inline_document(&fixture.state, command.clone())
            .await
            .context("exact actorless key must replay rather than enter fresh checksum dedup")?;
        assert!(second.mutation.replayed);
        assert_eq!(second.mutation.mutation.id, first.mutation.mutation.id);
        assert_eq!(second.mutation.job_id, first.mutation.job_id);
        assert_eq!(second.mutation.async_operation_id, first.mutation.async_operation_id);
        assert_eq!(second.mutation.result_revision_id, first.mutation.result_revision_id);
        assert_eq!(second.mutation.request_fingerprint, first.mutation.request_fingerprint);
        assert_eq!(admission_row_counts(&fixture).await?, before);
        let mut changed = command;
        changed.document_hint = Some("different semantic metadata".to_owned());
        assert!(
            matches!(
                service.upload_inline_document(&fixture.state, changed).await,
                Err(ApiError::IdempotencyConflict(_))
            ),
            "changed metadata must reach atomic fingerprint refusal"
        );
        assert_eq!(admission_row_counts(&fixture).await?, before);
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

async fn call_mcp_append(
    auth: &ironrag_backend::interfaces::http::auth::AuthContext,
    state: &ironrag_backend::app::state::AppState,
    request: ironrag_backend::mcp_types::McpUpdateDocumentRequest,
) -> Result<ironrag_backend::mcp_types::McpMutationReceipt> {
    use axum::{body::Body, extract::State, http::Request};
    use ironrag_backend::interfaces::http::mcp::{
        MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER, handle_diagnostics_jsonrpc,
    };
    let initialize = Request::post("/v1/mcp/diagnostics")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"jsonrpc":"2.0", "id":"initialize", "method":"initialize",
            "params":{"protocolVersion":MCP_PROTOCOL_VERSION, "capabilities":{},
                "clientInfo":{"name":"append-regression","version":"1"}}})
            .to_string(),
        ))?;
    let initialized =
        handle_diagnostics_jsonrpc(auth.clone(), State(state.clone()), initialize).await;
    assert_eq!(initialized.status(), axum::http::StatusCode::OK);
    let session = initialized
        .headers()
        .get(MCP_SESSION_HEADER)
        .context("MCP session header")?
        .to_str()?
        .to_owned();
    let call = Request::post("/v1/mcp/diagnostics")
        .header("content-type", "application/json")
        .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
        .header(MCP_SESSION_HEADER, session)
        .body(Body::from(
            serde_json::json!({"jsonrpc":"2.0", "id":"append", "method":"tools/call",
            "params":{"name":"create_document_revision", "arguments":{
                "library":request.library,"documentId":request.document_id,"mode":"append",
                "baseRevisionId":request.base_revision_id,"idempotencyKey":request.idempotency_key,
                "appendedText":request.appended_text}}})
            .to_string(),
        ))?;
    let response = handle_diagnostics_jsonrpc(auth.clone(), State(state.clone()), call).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let payload: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 64 * 1024).await?)?;
    assert_ne!(
        payload["result"]["isError"], true,
        "MCP append must reach atomic replay: {payload}"
    );
    serde_json::from_value(payload["result"]["structuredContent"].clone()).context("MCP receipt")
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

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_http_replacement_owns_complete_bytes_and_metadata() -> Result<()> {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let document = fixture.state.application_services.content.create_document(&fixture.state,
            CreateDocumentCommand { workspace_id: fixture.workspace_id, library_id: fixture.library_id,
                external_key: Some("bound-http-replacement".to_owned()), file_name: Some("restricted-old.txt".to_owned()),
                created_by_principal_id: None, parent_external_key: None }).await?;
        let token = crate::iam_token_support::mint_api_token(&fixture.state.persistence.postgres,
            Some(fixture.workspace_id), "workspace", "bound-http-replacement", &["documents:write"]).await?;
        let router = ironrag_backend::interfaces::http::content::router().with_state(fixture.state.clone());
        let path = format!("/content/documents/{}/revisions", document.id);
        let bytes = b"safe replacement";
        let descriptor = ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))),
            byte_size: bytes.len() as i64, mime_type: "text/plain".to_owned(), file_name: "safe.txt".to_owned(),
            title: None, language_code: Some("ru".to_owned()), source_uri: Some("https://example.invalid/safe".to_owned()),
            document_hint: Some("safe hint".to_owned()),
        };
        let intent_response = router.clone().oneshot(Request::post(format!("/content/documents/{}/visibility-intents", document.id))
            .header("content-type", "application/json").header("authorization", format!("Bearer {}", token.plaintext))
            .body(Body::from(serde_json::json!({"idempotencyKey":"http-denial", "sourceIdentity":"opaque-http-source",
                "materializationToken":null, "replacement":descriptor}).to_string()))?).await?;
        assert_eq!(intent_response.status(), StatusCode::CREATED);
        let receipt: ironrag_contracts::documents::VisibilityIntentReceipt = serde_json::from_slice(
            &axum::body::to_bytes(intent_response.into_body(), 64*1024).await?)?;
        let payload = serde_json::json!({"mode":"replace", "markdown":"safe replacement", "fileName":"safe.txt",
            "mimeType":" TEXT/PLAIN ", "title":null, "languageCode":"ru", "sourceUri":"https://example.invalid/safe",
            "documentHint":"safe hint", "visibilityIntentId":receipt.visibility_intent_id, "idempotencyKey":"http-bound-key"});
        let request = |body: serde_json::Value| Request::post(&path).header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", token.plaintext)).body(Body::from(body.to_string())).expect("request");
        let before = admission_row_counts(&fixture).await?;
        for removed in ["visibilityPolicy", "visibility_policy"] {
            let mut invalid = payload.clone(); invalid[removed] = serde_json::json!("suppressed_until_ready");
            assert_eq!(router.clone().oneshot(request(invalid)).await?.status(), StatusCode::BAD_REQUEST);
        }
        let mut unbound = payload.clone(); unbound.as_object_mut().context("payload")?.remove("visibilityIntentId");
        let denied = router.clone().oneshot(request(unbound)).await?;
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        let problem: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(denied.into_body(), 64*1024).await?)?;
        assert_eq!(problem["code"], "visibility_intent_required");
        let mut wrong_bytes = payload.clone(); wrong_bytes["markdown"] = serde_json::json!("different actual bytes");
        let denied = router.clone().oneshot(request(wrong_bytes)).await?;
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        let problem: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(denied.into_body(), 64*1024).await?)?;
        assert_eq!(problem["code"], "visibility_binding_conflict");
        assert_eq!(admission_row_counts(&fixture).await?, before);
        let created = router.clone().oneshot(request(payload.clone())).await?;
        assert_eq!(created.status(), StatusCode::ACCEPTED);
        let admission: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(created.into_body(), 64*1024).await?)?;
        let mutation_id: Uuid = serde_json::from_value(admission["mutation"]["id"].clone())?;
        let revision_id: Uuid = serde_json::from_value(admission["items"][0]["result_revision_id"].clone())?;
        let stored = ironrag_backend::infra::repositories::content_revision_repository::get_revision_replacement_identity(
            &fixture.state.persistence.postgres, revision_id).await?.context("snapshot")?;
        assert_eq!(stored, descriptor);
        let replay = router.clone().oneshot(request(payload.clone())).await?;
        assert_eq!(replay.status(), StatusCode::ACCEPTED);
        let repeated: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(replay.into_body(), 64*1024).await?)?;
        assert_eq!(repeated["mutation"]["id"], admission["mutation"]["id"]);
        let counts = admission_row_counts(&fixture).await?;
        let mut changed_metadata = payload.clone(); changed_metadata["title"] = serde_json::json!("another explicit title");
        let denied = router.clone().oneshot(request(changed_metadata)).await?;
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        let problem: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(denied.into_body(), 64*1024).await?)?;
        assert_eq!(problem["code"], "idempotency_conflict");
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        let artifact: String = sqlx::query_scalar("select storage_key from content_revision where id=$1")
            .bind(revision_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(fixture.state.content_storage.read_revision_source(&artifact).await?, bytes);
        let bound: Option<Uuid> = sqlx::query_scalar("select visibility_intent_id from content_mutation_item where mutation_id=$1")
            .bind(mutation_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(bound, Some(receipt.visibility_intent_id));
        // The same wire body on multipart must replay the original bundle.
        let multipart = format!("--boundary\r\nContent-Disposition: form-data; name=\"idempotency_key\"\r\n\r\nhttp-bound-key\r\n--boundary\r\nContent-Disposition: form-data; name=\"visibility_intent_id\"\r\n\r\n{}\r\n--boundary\r\nContent-Disposition: form-data; name=\"language_code\"\r\n\r\nru\r\n--boundary\r\nContent-Disposition: form-data; name=\"source_uri\"\r\n\r\nhttps://example.invalid/safe\r\n--boundary\r\nContent-Disposition: form-data; name=\"document_hint\"\r\n\r\nsafe hint\r\n--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"safe.txt\"\r\nContent-Type: text/plain\r\n\r\nsafe replacement\r\n--boundary--\r\n", receipt.visibility_intent_id);
        let multipart_request = |body: String| Request::post(&path).header("content-type", "multipart/form-data; boundary=boundary")
            .header("authorization", format!("Bearer {}", token.plaintext)).body(Body::from(body)).expect("multipart request");
        let repeated = router.clone().oneshot(multipart_request(multipart.clone())).await?;
        assert_eq!(repeated.status(), StatusCode::ACCEPTED);
        let repeated: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(repeated.into_body(), 64*1024).await?)?;
        assert_eq!(repeated["mutation"]["id"], admission["mutation"]["id"]);
        let removed = multipart.replace("--boundary--", "--boundary\r\nContent-Disposition: form-data; name=\"visibility_policy\"\r\n\r\nsuppressed_until_ready\r\n--boundary--");
        assert_eq!(router.oneshot(multipart_request(removed)).await?.status(), StatusCode::BAD_REQUEST);
        assert_eq!(admission_row_counts(&fixture).await?, counts);
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
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
    Ok(WebCaptureMaterializationAdmissionRequest {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        mutation_id: web.mutation.id,
        final_url: format!("https://example.invalid/materialized/{key}"),
        replacement: ironrag_contracts::documents::ReplacementIdentity {
            file_name: "materialized.txt".to_owned(),
            checksum: format!("sha256:{}", hex::encode(sha2::Sha256::digest(key.as_bytes()))),
            byte_size: i64::try_from(key.len())?,
            mime_type: "text/plain".to_owned(),
            title: Some("Materialized capture".to_owned()),
            language_code: None,
            source_uri: Some(format!("https://example.invalid/materialized/{key}")),
            document_hint: None,
        },
        storage_key: format!("fixture/materialized/{key}"),
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
          where mutation_id in (select subject_id from ops_async_operation
            where subject_kind='content_mutation' and parent_async_operation_id in
              (select id from ops_async_operation where subject_kind='content_web_ingest_run'
                and subject_id in (select id from content_web_ingest_run where mutation_id=$1)))",
    )
    .bind(request.mutation_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    let job_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_job
          where mutation_id in (select subject_id from ops_async_operation
            where subject_kind='content_mutation' and parent_async_operation_id in
              (select id from ops_async_operation where subject_kind='content_web_ingest_run'
                and subject_id in (select id from content_web_ingest_run where mutation_id=$1)))
           and job_kind = 'content_mutation'",
    )
    .bind(request.mutation_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    Ok((item_count, job_count))
}

fn captured_content_bundle(
    bundle: ironrag_backend::infra::repositories::admission_repository::WebCaptureMaterializationAdmissionBundle,
) -> Result<ironrag_backend::infra::repositories::admission_repository::ContentAdmissionBundle> {
    match bundle {
        ironrag_backend::infra::repositories::admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(bundle) => Ok(*bundle),
        _ => anyhow::bail!("expected captured page payload, not duplicate content"),
    }
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
#[ignore = "requires local postgres with required extensions"]
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
            .expect_err("missing target must be validated before key replay");
    assert!(matches!(conflict, AdmissionError::TargetDocumentNotFound { .. }));
    assert_eq!(admission_row_counts(&fixture).await?, counts);

    let second = fixture
        .state
        .application_services
        .content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some("real-second-replay-target".to_owned()),
                file_name: Some("other.txt".to_owned()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await?;
    conflicting.target = ContentAdmissionTarget::Existing { document_id: second.id };
    let second_counts = admission_row_counts(&fixture).await?;
    let conflict =
        admit_content_with_failpoint(&fixture.state.persistence.postgres, &conflicting, None)
            .await
            .expect_err("same key for a real other document must conflict");
    assert!(matches!(conflict, AdmissionError::IdempotencyConflict { .. }));
    assert_eq!(admission_row_counts(&fixture).await?, second_counts);

    fixture.cleanup().await
}

// The repair path this test guards is tolerance for real data, not dead code.
// It was deleted once as "admission-era legacy" and had to be reverted: a
// repeated delete must REPAIR a stale applied item, not answer 500. Before
// removing it again, prove there are zero mutation items with a null target
// left on every live stand — the count was five figures when this was last
// measured.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn repeated_delete_repairs_stale_metadata_for_the_exact_target() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
        let revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:stale-delete",
                    "Stale delete fixture",
                    Some("upload://stale-delete.txt"),
                ),
                "stale-delete.txt",
            )
            .await?;
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
            .await?;

        let accepted = content.admit_mutation(&fixture.state,AdmitMutationCommand {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id: document.id,
            operation_kind: "delete".to_owned(), idempotency_key: Some("stale-delete-replay".to_owned()),
            requested_by_principal_id: None, request_surface: "rest".to_owned(), source_identity: None,
            revision: None, parent_async_operation_id: None,
        }).await?;
        let operation_id = accepted.async_operation_id.context("delete operation")?;
        let stale_delete = accepted.mutation;
        let stale_item = accepted.items.into_iter().next().context("accepted delete item")?;
        assert_eq!(stale_item.base_revision_id,Some(revision.id));
        // Seed a crash before aggregate completion, with the immutable accepted
        // target and captured base intact. The retry must finish this receipt.
        sqlx::query("update content_mutation set mutation_state='accepted',completed_at=null where id=$1")
            .bind(stale_delete.id).execute(&fixture.state.persistence.postgres).await?;
        sqlx::query("update ops_async_operation set status='accepted',completed_at=null where id=$1")
            .bind(operation_id).execute(&fixture.state.persistence.postgres).await?;
        sqlx::query("update content_mutation_item set item_state='applied',message='stale applied item' where id=$1")
            .bind(stale_item.id).execute(&fixture.state.persistence.postgres).await?;

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
                    idempotency_key: Some("stale-delete-replay".to_owned()),
                    requested_by_principal_id: None,
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: None,
                    parent_async_operation_id: None,
                },
            )
            .await
            .context("an exact-target delete replay must repair stale item metadata")?;

        assert_eq!(admission.mutation.id, stale_delete.id);
        let item = admission.items.into_iter().next().context("repaired delete item is missing")?;
        assert_eq!(item.id, stale_item.id);
        assert_eq!(item.document_id, Some(document.id));
        assert_eq!(item.base_revision_id, Some(revision.id));
        assert_eq!(item.item_state, "applied");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
            .application_services
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
#[ignore = "requires local postgres with required extensions"]
async fn invalid_head_reference_is_an_integrity_error_not_a_retryable_conflict() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
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
        let foreign_revision = content_lifecycle_support::create_imported_revision(
                &fixture.state,
                revision_command(
                    source_document.id,
                    "upload",
                    "sha256:foreign-head-reference",
                    "Foreign head reference",
                    Some("upload://foreign-head-reference.txt"),
                ),
                "source.txt",
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
        let invalid_write = sqlx::query(
            "update content_document_head
             set active_revision_id = $2, readable_revision_id = $2
             where document_id = $1",
        )
        .bind(target_document.id)
        .bind(foreign_revision.id)
        .execute(&fixture.state.persistence.postgres)
        .await.expect_err("fresh writes must be rejected by the lineage foreign key");
        assert!(invalid_write.as_database_error().is_some(), "foreign revision ownership must be rejected");
        // Seed already-corrupt historical state in an isolated fixture to prove
        // admission's independent integrity guard; the production FK stays on.
        let mut corrupt_fixture = fixture.state.persistence.postgres.begin().await?;
        sqlx::query("set local session_replication_role = replica").execute(&mut *corrupt_fixture).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(target_document.id).bind(foreign_revision.id).execute(&mut *corrupt_fixture).await?;
        corrupt_fixture.commit().await?;
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
#[ignore = "requires local postgres with required extensions"]
async fn retention_pruned_attempt_pointer_does_not_block_revision_admission() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let content = &fixture.state.application_services.content;
        let document = content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(format!("pruned-attempt-{}", Uuid::now_v7())),
                    file_name: Some("pruned.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        let revision = content_lifecycle_support::create_imported_revision(
            &fixture.state,
            revision_command(
                document.id,
                "upload",
                "sha256:pruned-attempt-base",
                "Pruned attempt base",
                Some("upload://pruned-attempt-base.txt"),
            ),
            "pruned.txt",
        )
        .await?;
        // Simulate the snapshot-restore aftermath observed in production: the
        // head carries an operational attempt pointer whose row no longer
        // exists. The restore path writes with FK enforcement disabled
        // (session_replication_role = replica), so mirror that here.
        let dangling_attempt = Uuid::now_v7();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "set session_replication_role = replica;
             update content_document_head
             set active_revision_id = '{rev}',
                  readable_revision_id = '{rev}',
                 latest_successful_attempt_id = '{attempt}'
             where document_id = '{doc}';
             set session_replication_role = origin;",
            rev = revision.id,
            attempt = dangling_attempt,
            doc = document.id,
        )))
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let mut request = content_request(&fixture, "pruned-attempt-pointer");
        request.operation_kind = "replace".to_string();
        request.target = ContentAdmissionTarget::Existing { document_id: document.id };
        admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
            .await
            .expect("a retention-pruned attempt pointer must not block admission");

        let head_attempt = sqlx::query_scalar::<_, Option<Uuid>>(
            "select latest_successful_attempt_id from content_document_head
             where document_id = $1",
        )
        .bind(document.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(head_attempt, None, "dangling operational pointer must be written back as null");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn projection_promotion_reports_reference_integrity_instead_of_missing_document() -> Result<()>
{
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
                    external_key: Some(format!("promotion-integrity-{}", Uuid::now_v7())),
                    file_name: Some("document.txt".to_string()),
                    created_by_principal_id: None,
                    parent_external_key: None,
                },
            )
            .await?;
        let head_before = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document.id,
        )
        .await?
        .context("document head missing before invalid promotion")?;

        let error = fixture
            .state
            .application_services
            .content
            .promote_document_head(
                &fixture.state,
                PromoteHeadCommand {
                    document_id: document.id,
                    active_revision_id: None,
                    readable_revision_id: None,
                    publishing_mutation_item_id: None,
                    latest_mutation_id: Some(Uuid::now_v7()),
                    latest_successful_attempt_id: None,
                },
            )
            .await
            .expect_err("invalid head ownership must be an integrity error");
        assert!(matches!(error, ApiError::Internal));

        let head_after = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document.id,
        )
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
#[ignore = "requires local postgres with required extensions"]
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
            .application_services
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
            .application_services
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
        let second_head_before = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing before delete replay")?;
        let idempotency_key = format!("delete-target-{}", Uuid::now_v7());

        let first_admission = fixture
            .state
            .application_services
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
            .application_services
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
            .application_services
            .content
            .get_mutation_admission(&fixture.state, first_admission.mutation.id)
            .await?;
        assert_eq!(original_admission.items.len(), 1);
        assert_eq!(original_admission.items[0].id, original_item.id);
        assert_eq!(original_admission.items[0].document_id, Some(first_document.id));
        let second_document_after = content_document_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document disappeared after conflicting delete")?;
        assert_eq!(second_document_after.document_state, "active");
        assert!(second_document_after.deleted_at.is_none());
        let second_head_after = content_document_head_repository::get_document_head(
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
#[ignore = "requires local postgres with required extensions"]
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
            .application_services
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
            .application_services
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
        let second_head_before = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document head missing before keyed replay")?;

        let initial_delete = fixture
            .state
            .application_services
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
            .application_services
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
            .application_services
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

        let second_document_after = content_document_repository::get_document_by_id(
            &fixture.state.persistence.postgres,
            second_document.id,
        )
        .await?
        .context("second document disappeared after keyed replay conflict")?;
        assert_eq!(second_document_after.document_state, "active");
        assert!(second_document_after.deleted_at.is_none());
        let second_head_after = content_document_head_repository::get_document_head(
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
#[ignore = "requires local postgres with required extensions"]
async fn incomplete_content_replay_fails_closed_without_recreating_a_job() -> Result<()> {
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

    let error = admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None)
        .await
        .expect_err("an incomplete replay graph must fail closed");
    assert!(matches!(
        error,
        AdmissionError::InvariantViolation("linked job must match payload presence")
    ));
    let job_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from ingest_job where mutation_id = $1",
    )
    .bind(created.mutation.id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    assert_eq!(job_count, 0, "replay must not recreate missing lifecycle rows");

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
    let error = admit_web_run_with_failpoint(&fixture.state.persistence.postgres, &request, None)
        .await
        .expect_err("an incomplete web replay graph must fail closed");
    assert!(matches!(error, AdmissionError::InvariantViolation("web seed is missing")));
    let seed_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from content_web_discovered_page where run_id = $1",
    )
    .bind(first.run.id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await?;
    assert_eq!(seed_count, 0, "replay must not recreate missing web lifecycle rows");

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
#[ignore = "requires local postgres with required extensions"]
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
    let first = captured_content_bundle(created.into_bundle())?;
    assert!(first.is_complete());
    let item = first.item.as_ref().context("child item")?;
    let job = first.job.as_ref().context("child job")?;
    let revision = first.revision.as_ref().context("child revision")?;
    assert_ne!(first.mutation.id, request.mutation_id);
    assert_eq!(item.mutation_id, first.mutation.id);
    assert_eq!(item.document_id, Some(first.document.id));
    assert_eq!(item.result_revision_id, Some(revision.id));
    assert_eq!(item.item_state, "pending");
    assert_eq!(job.mutation_id, Some(first.mutation.id));
    assert_eq!(job.mutation_item_id, Some(item.id));
    assert_eq!(job.knowledge_document_id, Some(first.document.id));
    assert_eq!(job.knowledge_revision_id, Some(revision.id));
    assert_eq!(job.job_kind, "content_mutation");
    let queue: String = sqlx::query_scalar("select queue_state::text from ingest_job where id=$1")
        .bind(job.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
    assert_eq!(queue, "queued");
    let head = content_document_head_repository::get_document_head(
        &fixture.state.persistence.postgres,
        first.document.id,
    )
    .await?
    .context("child head")?;
    assert_eq!(head.latest_mutation_id, Some(first.mutation.id));
    assert_eq!(first.async_operation.parent_async_operation_id,
        Some(sqlx::query_scalar::<_,Uuid>("select id from ops_async_operation where subject_kind='content_web_ingest_run' and subject_id=(select id from content_web_ingest_run where mutation_id=$1)")
            .bind(request.mutation_id).fetch_one(&fixture.state.persistence.postgres).await?));

    let replay = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await?;
    assert!(matches!(replay, AdmissionOutcome::Replayed(_)));
    let replayed = captured_content_bundle(replay.into_bundle())?;
    assert_eq!(replayed.item.as_ref().context("replayed item")?.id, item.id);
    assert_eq!(replayed.job.as_ref().context("replayed job")?.id, job.id);
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (1, 1));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn web_capture_materialization_rolls_back_every_owned_write_without_failing_the_run()
-> Result<()> {
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
        let request = web_capture_materialization_request(
            &fixture,
            &format!("materialization-rollback-{failpoint:?}"),
        )
        .await?;
        let before = admission_row_counts(&fixture).await?;

        let error = admit_web_capture_materialization_with_failpoint(
            &fixture.state.persistence.postgres,
            &request,
            Some(failpoint),
        )
        .await
        .expect_err("injected materialization failure must abort the transaction");
        assert!(matches!(error, AdmissionError::InjectedFailure(point) if point == failpoint));
        assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));
        assert_eq!(admission_row_counts(&fixture).await?, before);
        let shell: bool = sqlx::query_scalar("select exists (select 1 from content_document where library_id=$1 and external_key=$2)")
            .bind(fixture.library_id).bind(&request.final_url).fetch_one(&fixture.state.persistence.postgres).await?;
        assert!(!shell, "failed atomic materialization must not leave a document shell");
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
#[ignore = "requires local postgres with required extensions"]
async fn web_capture_materialization_rejects_cross_scope_parent_without_partial_rows() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;
    let mut request =
        web_capture_materialization_request(&fixture, "materialization-identity").await?;
    // There is no caller-supplied precreated revision in atomic materialization.
    // Scope mismatch must be refused before any child identity is written.
    request.workspace_id = Uuid::now_v7();
    let before = admission_row_counts(&fixture).await?;

    let error = admit_web_capture_materialization_with_failpoint(
        &fixture.state.persistence.postgres,
        &request,
        None,
    )
    .await
    .expect_err("cross-scope parent must fail exact admission validation");
    assert!(matches!(error, AdmissionError::InvariantViolation(_)));
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (0, 0));
    assert_eq!(admission_row_counts(&fixture).await?, before);

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn web_capture_materialization_rejects_a_divergent_revision_projection_without_partial_rows()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let request =
        web_capture_materialization_request(&fixture, "materialization-projection").await?;
    let admitted = captured_content_bundle(
        admit_web_capture_materialization_with_failpoint(
            &fixture.state.persistence.postgres,
            &request,
            None,
        )
        .await?
        .into_bundle(),
    )?;
    let revision_id = admitted.revision.as_ref().context("captured revision")?.id;
    sqlx::query(
        "update knowledge_revision
         set checksum = $2
         where revision_id = $1",
    )
    .bind(revision_id)
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
    assert_eq!(materialization_owned_row_counts(&fixture, &request).await?, (1, 1));

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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

/// Accepts an independent denial and then its exactly bound replacement.
/// Returns `(document_id, payload_mutation_id, base_revision_id)`.
async fn admit_required_suppression(
    fixture: &ContentLifecycleFixture,
) -> Result<(Uuid, Uuid, Uuid)> {
    admit_existing_replacement(fixture, true).await
}

async fn admit_existing_replacement(
    fixture: &ContentLifecycleFixture,
    accept_intent: bool,
) -> Result<(Uuid, Uuid, Uuid)> {
    let content = &fixture.state.application_services.content;
    let document = content
        .create_document(
            &fixture.state,
            CreateDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: Some(format!("required-suppress-{}", Uuid::now_v7().simple())),
                file_name: Some("required-suppress.txt".to_string()),
                created_by_principal_id: None,
                parent_external_key: None,
            },
        )
        .await?;
    let base_revision = content_lifecycle_support::create_imported_revision(
        &fixture.state,
        revision_command(
            document.id,
            "upload",
            "sha256:required-suppression-base",
            "Required suppression base",
            Some("upload://required-suppress.txt"),
        ),
        "required-suppress.txt",
    )
    .await?;
    content
        .promote_document_head(
            &fixture.state,
            PromoteHeadCommand {
                publishing_mutation_item_id: None,
                document_id: document.id,
                active_revision_id: Some(base_revision.id),
                readable_revision_id: Some(base_revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await?;

    let mut request = content_request(fixture, "required-suppression-replace");
    request.operation_kind = "replace".to_string();
    request.target = ContentAdmissionTarget::Existing { document_id: document.id };
    if accept_intent {
        let intent_id = admit_replacement_intent(fixture, &request, "required-observation").await?;
        request.revision.as_mut().context("replacement revision")?.visibility_intent_id =
            Some(intent_id);
    }
    let admission =
        admit_content_with_failpoint(&fixture.state.persistence.postgres, &request, None).await?;
    let bundle = admission.into_bundle();
    Ok((document.id, bundle.mutation.id, base_revision.id))
}

async fn admit_replacement_intent(
    fixture: &ContentLifecycleFixture,
    payload: &ContentAdmissionRequest,
    key: &str,
) -> Result<Uuid> {
    use ironrag_backend::infra::repositories::admission_repository::{
        VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint,
    };
    let ContentAdmissionTarget::Existing { document_id } = payload.target else {
        anyhow::bail!("intent fixture requires an existing document");
    };
    let intent = VisibilityAdmissionRequest {
        workspace_id: payload.workspace_id,
        library_id: payload.library_id,
        document_id,
        requested_by_principal_id: payload.requested_by_principal_id,
        request_surface: payload.request_surface.clone(),
        intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
            idempotency_key: key.to_owned(),
            source_identity: payload.source_identity.clone().context("intent source identity")?,
            materialization_token: None,
            replacement: payload
                .revision
                .as_ref()
                .context("intent replacement")?
                .replacement
                .clone(),
        },
    };
    Ok(admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &intent, None)
        .await?
        .into_bundle()
        .receipt
        .visibility_intent_id)
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn visibility_contract_required_busy_payload_denies_previous_content() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let content = &fixture.state.application_services.content;
        let document = content.create_document(&fixture.state, CreateDocumentCommand {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id,
            external_key: Some("visibility-contract-document".to_owned()),
            file_name: Some("old.txt".to_owned()), created_by_principal_id: None, parent_external_key: None,
        }).await?;
        let document_id = document.id;
        let base_revision = content_lifecycle_support::create_imported_revision(&fixture.state,
            revision_command(document_id, "upload", "sha256:old", "Old content", None),
            "old.txt",
        ).await?;
        let base_revision_id = base_revision.id;
        sqlx::query("update content_document_head set active_revision_id = $2, readable_revision_id = $2,
            latest_mutation_id = null where document_id = $1")
            .bind(document_id).bind(base_revision_id)
            .execute(&fixture.state.persistence.postgres).await?;
        let mut foreign = content_request(&fixture, "foreign-payload");
        foreign.operation_kind = "replace".to_owned();
        foreign.target = ContentAdmissionTarget::Existing { document_id };
        let foreign = admit_content_with_failpoint(&fixture.state.persistence.postgres, &foreign, None).await?.into_bundle();
        let foreign_mutation_id = foreign.mutation.id;
        sqlx::query("update knowledge_revision set text_state = 'text_readable', text_readable_at = now() where revision_id = $1")
            .bind(base_revision_id).execute(&fixture.state.persistence.postgres).await?;
        let visible_before: Option<Uuid> = sqlx::query_scalar("select revision_id from content_current_revision where document_id = $1")
            .bind(document_id).fetch_optional(&fixture.state.persistence.postgres).await?;
        assert_eq!(visible_before, Some(base_revision_id), "A is publicly current before denial");
        let before = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?
        .context("head missing")?;
        assert_eq!(before.readable_revision_id, Some(base_revision_id));
        let request = ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id: fixture.library_id, document_id,
            requested_by_principal_id: None, request_surface: "rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: "required-while-busy".to_owned(), source_identity: "source".to_owned(),
                materialization_token: None,
                replacement: ironrag_contracts::documents::ReplacementIdentity {
                    checksum: format!("sha256:{}", "a".repeat(64)), byte_size: 64,
                    mime_type: "text/plain".to_owned(), file_name: "safe.txt".to_owned(),
                    title: None, language_code: None, source_uri: None, document_hint: None,
                },
            },
        };
        use ironrag_backend::infra::repositories::admission_repository::admit_visibility_intent_with_failpoint;
        let created = admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &request, None)
            .await?.into_bundle();
        assert_eq!(created.receipt.revision_floor, 2, "floor includes the pending foreign revision");
        let replayed = admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &request, None).await?;
        assert!(matches!(replayed, AdmissionOutcome::Replayed(_)));
        assert_eq!(replayed.into_bundle().receipt, created.receipt);
        let mut changed = request.clone();
        changed.intent.replacement.title = Some("different request metadata".to_owned());
        let conflict = admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &changed, None)
            .await.expect_err("same key with different complete identity must conflict");
        assert!(matches!(conflict, AdmissionError::IdempotencyConflict { .. }));
        let after = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?
        .context("head missing")?;
        assert_eq!(after.latest_mutation_id, Some(foreign_mutation_id));
        assert_eq!(
            after.readable_revision_id, None,
            "REQUIRED protection must deny A even while foreign B owns the payload"
        );
        let no_job: i64 = sqlx::query_scalar("select count(*) from ingest_job where mutation_id = $1")
            .bind(created.mutation.id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(no_job, 0);
        let visible_after: Option<Uuid> = sqlx::query_scalar("select revision_id from content_current_revision where document_id = $1")
            .bind(document_id).fetch_optional(&fixture.state.persistence.postgres).await?;
        assert_eq!(visible_after, None, "accepted intent removes A from the canonical current view");
        let mut next = request.clone();
        next.intent.idempotency_key = "next-observation".to_owned();
        let before_rollback = admission_row_counts(&fixture).await?;
        let failure = admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &next,
            Some(AdmissionFailpoint::BeforeCommit)).await.expect_err("injected rollback");
        assert!(matches!(failure, AdmissionError::InjectedFailure(AdmissionFailpoint::BeforeCommit)));
        assert_eq!(admission_row_counts(&fixture).await?, before_rollback);
        let newer = admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &next, None)
            .await?.into_bundle();
        admit_visibility_intent_with_failpoint(&fixture.state.persistence.postgres, &request, None).await?;
        let current: Uuid = sqlx::query_scalar("select visibility_intent_id from content_document_head where document_id = $1")
            .bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(current, newer.receipt.visibility_intent_id, "old replay must not replace a newer obligation");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn required_intent_nulls_both_projections_and_binds_payload() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let (document_id, mutation_id, _base_revision_id) =
            admit_required_suppression(&fixture).await?;

        let (head_active, head_readable, head_latest): (Option<Uuid>, Option<Uuid>, Option<Uuid>) =
            sqlx::query_as(
                "select active_revision_id, readable_revision_id, latest_mutation_id
                 from content_document_head
                 where document_id = $1",
            )
            .bind(document_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(head_active, None, "REQUIRED suppression must clear the active head pointer");
        assert_eq!(
            head_readable, None,
            "REQUIRED suppression must clear the readable head pointer"
        );
        assert_eq!(head_latest, Some(mutation_id));

        let (projection_active, projection_readable): (Option<Uuid>, Option<Uuid>) =
            sqlx::query_as(
                "select active_revision_id, readable_revision_id
                 from knowledge_document
                 where document_id = $1",
            )
            .bind(document_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(projection_active, None, "the query projection must clear the active pointer");
        assert_eq!(
            projection_readable, None,
            "the query projection must clear the readable pointer"
        );

        let (intent_id, bound_id, intent_state): (Uuid, Uuid, String) = sqlx::query_as(
            "select h.visibility_intent_id, payload.visibility_intent_id, obligation.item_state::text
             from content_document_head h
             join content_mutation_item payload on payload.document_id=h.document_id and payload.mutation_id=$1
             join content_mutation_item obligation on obligation.id=h.visibility_intent_id
             where h.document_id=$2",
        )
        .bind(mutation_id)
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(intent_id, bound_id, "payload is bound to the independently applied obligation");
        assert_eq!(intent_state, "applied");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn terminal_failed_suppression_is_not_repromoted_by_maintenance() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let (document_id, mutation_id, base_revision_id) =
            admit_required_suppression(&fixture).await?;

        // A chunk-bearing revision exists, so the null-head repair pass has a
        // real candidate; the independent intent must refuse its stale bytes.
        sqlx::query(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum
             ) values ($1, $2, 0, 0, 16, 4, 'suppressed base chunk', 'sha256:suppressed-base')",
        )
        .bind(Uuid::now_v7())
        .bind(base_revision_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        sqlx::query(
            "update content_mutation
             set mutation_state = 'failed',
                 completed_at = now(),
                 failure_code = 'synthetic_terminal_failure'
             where id = $1",
        )
        .bind(mutation_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let report = ironrag_backend::services::maintenance::repair::promote_null_heads(
            &fixture.state,
            Some(fixture.library_id),
        )
        .await?;
        assert_eq!(
            report.promoted, 0,
            "a suppressed document must never be repromoted after a terminal failure",
        );

        let auto = ironrag_backend::services::maintenance::repair::promote_null_heads_auto(
            &fixture.state,
            Some(fixture.library_id),
        )
        .await?;
        assert_eq!(auto.promoted, 0, "auto repair must respect terminal suppression");
        assert_eq!(auto.candidates_considered, 0, "suppression must be excluded before recovery bookkeeping");

        let mut retry = content_request(&fixture, "bound-retry-after-denial");
        retry.operation_kind = "reprocess".to_string();
        retry.target = ContentAdmissionTarget::Existing { document_id };
        let denied = admit_content_with_failpoint(&fixture.state.persistence.postgres, &retry, None)
            .await.expect_err("unbound reprocess cannot bypass the current intent");
        assert!(matches!(denied, AdmissionError::VisibilityIntentRequired { .. }));
        retry.operation_kind = "replace".to_owned();
        retry.revision.as_mut().context("retry revision")?.replacement =
            content_request(&fixture, "required-suppression-replace")
                .revision.context("original replacement")?.replacement;
        retry.revision.as_mut().context("retry revision")?.visibility_intent_id =
            sqlx::query_scalar("select visibility_intent_id from content_document_head where document_id=$1")
                .bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        let retry = admit_content_with_failpoint(&fixture.state.persistence.postgres, &retry, None)
            .await?
            .into_bundle();
        let report = ironrag_backend::services::maintenance::repair::promote_null_heads(
            &fixture.state,
            Some(fixture.library_id),
        )
        .await?;
        assert_eq!(report.promoted, 0, "bound payload admission cannot lift denial before READY");
        sqlx::query("update content_mutation set mutation_state = 'failed' where id = $1")
            .bind(retry.mutation.id).execute(&fixture.state.persistence.postgres).await?;
        let mut metadata = content_request(&fixture, "metadata-after-suppression");
        metadata.operation_kind = "reprocess".to_string();
        metadata.target = ContentAdmissionTarget::Existing { document_id };
        metadata.revision = None;
        let metadata_error = admit_content_with_failpoint(&fixture.state.persistence.postgres, &metadata, None)
            .await.expect_err("metadata-only admission cannot discharge the current intent");
        assert!(matches!(metadata_error, AdmissionError::VisibilityIntentRequired { .. }));
        let report = ironrag_backend::services::maintenance::repair::promote_null_heads(
            &fixture.state, Some(fixture.library_id),
        ).await?;
        assert_eq!(report.promoted, 0, "an applied metadata-only item cannot release suppression");
        let auto = ironrag_backend::services::maintenance::repair::promote_null_heads_auto(
            &fixture.state, Some(fixture.library_id),
        ).await?;
        assert_eq!(auto.candidates_considered, 0, "later admissions cannot erase durable suppression");
        fixture.state.application_services.content.promote_document_head(
            &fixture.state,
            PromoteHeadCommand {
                publishing_mutation_item_id: None,
                document_id,
                active_revision_id: Some(base_revision_id),
                readable_revision_id: Some(base_revision_id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        ).await.expect_err("unfenced promotion cannot bypass durable suppression");

        let (head_readable, head_latest): (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select readable_revision_id, latest_mutation_id
             from content_document_head
             where document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(head_readable, None, "maintenance must leave the head unreadable");
        assert_eq!(head_latest, Some(retry.mutation.id));
        let projection: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select active_revision_id, readable_revision_id from knowledge_document where document_id = $1",
        ).bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(projection, (None, None), "later admissions and repair must preserve the query projection");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn manual_repair_rechecks_suppression_after_candidate_read() -> Result<()> {
    #[derive(Debug, PartialEq, sqlx::FromRow)]
    struct RepairRacePointers {
        active_revision_id: Option<Uuid>,
        readable_revision_id: Option<Uuid>,
        latest_mutation_id: Option<Uuid>,
        projection_active_revision_id: Option<Uuid>,
        projection_readable_revision_id: Option<Uuid>,
    }
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let (document_id, mutation_id, base_revision_id) = admit_existing_replacement(&fixture, false).await?;
        let pool = &fixture.state.persistence.postgres;
        // A malformed but initially repairable head, with real historical chunks.
        sqlx::query("update content_mutation set mutation_state = 'failed' where id = $1")
            .bind(mutation_id).execute(pool).await?;
        fixture.state.application_services.content.promote_document_head(&fixture.state, PromoteHeadCommand {
            document_id, active_revision_id: None, readable_revision_id: None,
            publishing_mutation_item_id: None, latest_mutation_id: None, latest_successful_attempt_id: None,
        }).await?;
        sqlx::query(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum
             ) values ($1, $2, 0, 0, 16, 4, 'repair race base', 'sha256:repair-race')",
        )
        .bind(Uuid::now_v7()).bind(base_revision_id).execute(pool).await?;
        let (selected_tx, selected_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let repair = tokio::spawn({
            let state = fixture.state.clone();
            let library_id = fixture.library_id;
            async move {
                ironrag_backend::services::maintenance::repair::promote_null_heads_after_candidates_for_test(
                    &state, Some(library_id), async move {
                        let _ = selected_tx.send(());
                        let _ = release_rx.await;
                    },
                ).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(30), selected_rx).await??;
        let mut replacement = content_request(&fixture, "suppress-after-repair-selected");
        replacement.operation_kind = "replace".to_string();
        replacement.target = ContentAdmissionTarget::Existing { document_id };
        let intent_id = admit_replacement_intent(&fixture, &replacement, "deny-after-candidate-selection").await?;
        replacement.revision.as_mut().context("race replacement")?.visibility_intent_id = Some(intent_id);
        let replacement = admit_content_with_failpoint(pool, &replacement, None).await?.into_bundle();
        release_tx.send(()).expect("repair must still be paused");
        let report = tokio::time::timeout(std::time::Duration::from_secs(30), repair).await???;
        assert_eq!(report.promoted, 0, "stale repair candidate cannot undo concurrent suppression");
        let pointers: RepairRacePointers = sqlx::query_as(
            "select h.active_revision_id, h.readable_revision_id, h.latest_mutation_id,
                    k.active_revision_id as projection_active_revision_id,
                    k.readable_revision_id as projection_readable_revision_id
             from content_document_head h join knowledge_document k on k.document_id = h.document_id
             where h.document_id = $1",
        ).bind(document_id).fetch_one(pool).await?;
        assert_eq!(pointers, RepairRacePointers {
            active_revision_id: None,
            readable_revision_id: None,
            latest_mutation_id: Some(replacement.mutation.id),
            projection_active_revision_id: None,
            projection_readable_revision_id: None,
        });
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}
