#![cfg(feature = "test-support")]

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
};
use ironrag_backend::{
    interfaces::http::knowledge::{
        create_graph_preparation, get_graph_preparation, get_graph_preparation_result,
    },
    services::knowledge::preparation::{self, PreparationStatus},
};
use uuid::Uuid;

use crate::{
    content_lifecycle_support::ContentLifecycleFixture,
    graph_visibility_race::{graph_auth, prepare_graph, seed_published_graph},
    iam_token_support::scoped_workspace_auth,
};

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn held_claim_deduplicates_and_status_does_not_validate_graph() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let (document, nodes, _) = seed_published_graph(&fixture).await?;
        let snapshot = ironrag_backend::infra::repositories::get_runtime_graph_snapshot(&fixture.state.persistence.postgres, fixture.library_id).await?.context("snapshot")?;
        let auth = graph_auth(&fixture).await?;
        let (first, second) = tokio::try_join!(
            preparation::admit(&fixture.state, fixture.library_id),
            preparation::admit(&fixture.state, fixture.library_id),
        )?;
        ensure!(first.id == second.id);
        let claim = preparation::claim_next(&fixture.state).await?.context("first claim")?;
        let other = ironrag_backend::infra::repositories::catalog_repository::create_library(
            &fixture.state.persistence.postgres, fixture.workspace_id, "other-pending-graph", "Other", None, None,
        ).await?;
        let _other_preparation = preparation::admit(&fixture.state, other.id).await?;
        ensure!(preparation::claim_next(&fixture.state).await?.is_none());
        sqlx::query("insert into runtime_graph_evidence (id,library_id,evidence_identity_key,target_kind,target_id,document_id,revision_id,evidence_text) values ($1,$2,'poll-stale','node',$3,$4,null,'stale')")
            .bind(Uuid::now_v7()).bind(fixture.library_id).bind(nodes[0]).bind(document).execute(&fixture.state.persistence.postgres).await?;
        ensure!(!ironrag_backend::infra::repositories::content_fingerprint_repository::graph_projection_is_current(&fixture.state.persistence.postgres, snapshot.projection_identity()).await?);
        let post = tokio::time::timeout(Duration::from_secs(2), create_graph_preparation(auth.clone(), State(fixture.state.clone()), Path(fixture.library_id))).await??;
        ensure!(post.status() == axum::http::StatusCode::ACCEPTED);
        let status = tokio::time::timeout(Duration::from_secs(2), get_graph_preparation(auth, State(fixture.state.clone()), Path((fixture.library_id, first.id)))).await??;
        let bytes = axum::body::to_bytes(status.into_body(), 65536).await?;
        let observed: preparation::KnowledgeGraphPreparationResponse = serde_json::from_slice(&bytes)?;
        ensure!(observed.id == first.id && observed.status == PreparationStatus::Validating && observed.attempts == 1);
        preparation::finish_error(&fixture.state, &claim, "fixture_complete").await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn expired_attempts_are_fenced_and_eventually_fail() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let initial = prepare_graph(&fixture, graph_auth(&fixture).await?).await?;
        let artifact = preparation::result_record(&fixture.state, fixture.library_id, initial.id).await?.artifact.context("initial artifact")?;
        sqlx::query("update catalog_library set source_truth_version=source_truth_version+1 where id=$1")
            .bind(fixture.library_id).execute(&fixture.state.persistence.postgres).await?;
        let admitted = preparation::admit(&fixture.state, fixture.library_id).await?;
        let first = preparation::claim_next(&fixture.state).await?.context("initial claim")?;
        for attempt in 1..=3 {
            sqlx::query("update maintenance_job_run set cursor_json=jsonb_set(cursor_json,'{active,heartbeat_at}',to_jsonb($2::text)) where id=$1")
                .bind(first.row_id).bind((chrono::Utc::now()-chrono::Duration::minutes(3)).to_rfc3339()).execute(&fixture.state.persistence.postgres).await?;
            let _ = preparation::claim_next(&fixture.state).await?;
            let status = preparation::status(&fixture.state, fixture.library_id, admitted.id).await?;
            ensure!(status.attempts == attempt);
            if attempt == 3 { ensure!(status.status == PreparationStatus::Failed); break; }
            ensure!(status.status == PreparationStatus::Retrying);
            sqlx::query("update maintenance_job_run set cursor_json=jsonb_set(cursor_json,'{records,1,retry_at}',to_jsonb($2::text)) where id=$1")
                .bind(first.row_id).bind((chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339()).execute(&fixture.state.persistence.postgres).await?;
            ensure!(preparation::claim_next(&fixture.state).await?.is_some());
        }
        ensure!(!preparation::finish_success(&fixture.state, &first, artifact).await?);
        Ok::<_, anyhow::Error>(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn ready_result_has_fixed_expiry_and_missing_artifact_fails_explicitly() -> Result<()> {
    let mut fixture = ContentLifecycleFixture::create().await?;
    fixture.state.settings.maintenance_enabled = false;
    let result = Box::pin(async {
        seed_published_graph(&fixture).await?;
        let auth = graph_auth(&fixture).await?;
        let descriptor = prepare_graph(&fixture, auth.clone()).await?;
        let second = preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?;
        ensure!(descriptor.expires_at == second.expires_at);
        let record =
            preparation::result_record(&fixture.state, fixture.library_id, descriptor.id).await?;
        let manifest = record.artifact.context("artifact manifest")?;
        let path = std::path::Path::new(&fixture.state.settings.content_storage_root)
            .join(&manifest.storage_key);
        tokio::fs::remove_file(path).await?;
        ensure!(
            Box::pin(get_graph_preparation_result(
                auth,
                State(fixture.state.clone()),
                Path((fixture.library_id, descriptor.id))
            ))
            .await
            .is_err()
        );
        let failed = preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?;
        ensure!(
            failed.status == PreparationStatus::Failed
                && failed.error_code.as_deref() == Some("artifact_read_failed")
        );
        Ok::<_, anyhow::Error>(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn real_readiness_loss_and_restoration_supersede_ready_receipt() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let (document, _, _) = seed_published_graph(&fixture).await?;
        let auth = graph_auth(&fixture).await?;
        let descriptor = prepare_graph(&fixture, auth.clone()).await?;
        let revision = ironrag_backend::infra::repositories::content_fingerprint_repository::get_current_revision_id(&fixture.state.persistence.postgres, document).await?.context("current revision")?;
        let now = chrono::Utc::now();
        fixture.state.document_store.update_revision_readiness(ironrag_backend::infra::knowledge_plane::RevisionReadinessUpdate {
            revision_id: revision, text_state: "accepted", vector_state: "ready", graph_state: "ready",
            text_readable_at: None, vector_ready_at: Some(now), graph_ready_at: Some(now), superseded_by_revision_id: None,
        }).await?.context("revision readiness loss")?;
        ensure!(preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?.status == PreparationStatus::Superseded);
        ensure!(Box::pin(get_graph_preparation_result(auth.clone(), State(fixture.state.clone()), Path((fixture.library_id, descriptor.id)))).await.is_err());
        fixture.state.document_store.update_revision_readiness(ironrag_backend::infra::knowledge_plane::RevisionReadinessUpdate {
            revision_id: revision, text_state: "text_readable", vector_state: "ready", graph_state: "ready",
            text_readable_at: Some(now), vector_ready_at: Some(now), graph_ready_at: Some(now), superseded_by_revision_id: None,
        }).await?.context("revision readiness restoration")?;
        ensure!(preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?.status == PreparationStatus::Superseded);
        ensure!(Box::pin(get_graph_preparation_result(auth, State(fixture.state.clone()), Path((fixture.library_id, descriptor.id)))).await.is_err());
        Ok::<_, anyhow::Error>(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn real_alias_write_supersedes_ready_receipt() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let (_, nodes, _) = seed_published_graph(&fixture).await?;
        let auth = graph_auth(&fixture).await?;
        let descriptor = prepare_graph(&fixture, auth.clone()).await?;
        let changed = ironrag_backend::infra::repositories::runtime_graph_repository::add_runtime_graph_node_aliases(
            &fixture.state.persistence.postgres, nodes[0], 1, &["fresh alias".to_owned()],
        ).await?;
        ensure!(changed == 1);
        ensure!(preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?.status == PreparationStatus::Superseded);
        ensure!(Box::pin(get_graph_preparation_result(auth, State(fixture.state.clone()), Path((fixture.library_id, descriptor.id)))).await.is_err());
        Ok::<_, anyhow::Error>(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn cleanup_retries_owned_debt_after_storage_failure() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        seed_published_graph(&fixture).await?;
        let descriptor = prepare_graph(&fixture, graph_auth(&fixture).await?).await?;
        let record = preparation::result_record(&fixture.state, fixture.library_id, descriptor.id).await?;
        let manifest = record.artifact.context("artifact")?;
        let path = std::path::Path::new(&fixture.state.settings.content_storage_root).join(&manifest.storage_key);
        let saved = path.with_extension("saved");
        tokio::fs::rename(&path, &saved).await?;
        tokio::fs::create_dir(&path).await?;
        let pg = &fixture.state.persistence.postgres;
        sqlx::query("update maintenance_job_run set cursor_json=jsonb_set(jsonb_set(cursor_json,'{records,0,expires_at}',to_jsonb($2::text)),'{debts,0,not_before}',to_jsonb($2::text)) where scope_id=$1 and class='graph.topology-preparation'")
            .bind(fixture.library_id).bind((chrono::Utc::now()-chrono::Duration::minutes(1)).to_rfc3339()).execute(pg).await?;
        preparation::cleanup_due(&fixture.state).await?;
        let debt: i64 = sqlx::query_scalar("select jsonb_array_length(cursor_json->'debts')::bigint from maintenance_job_run where scope_id=$1 and class='graph.topology-preparation'").bind(fixture.library_id).fetch_one(pg).await?;
        ensure!(debt == 1, "failed deletion must retain its debt");
        tokio::fs::remove_dir(&path).await?;
        tokio::fs::rename(&saved, &path).await?;
        sqlx::query("update maintenance_job_run set cursor_json=jsonb_set(cursor_json,'{debts,0,not_before}',to_jsonb($2::text)) where scope_id=$1 and class='graph.topology-preparation'")
            .bind(fixture.library_id).bind((chrono::Utc::now()-chrono::Duration::minutes(1)).to_rfc3339()).execute(pg).await?;
        preparation::cleanup_due(&fixture.state).await?;
        let debt: i64 = sqlx::query_scalar("select jsonb_array_length(cursor_json->'debts')::bigint from maintenance_job_run where scope_id=$1 and class='graph.topology-preparation'").bind(fixture.library_id).fetch_one(pg).await?;
        ensure!(debt == 0 && !tokio::fs::try_exists(path).await?);
        ensure!(preparation::status(&fixture.state, fixture.library_id, descriptor.id).await?.status == PreparationStatus::Expired);
        Ok::<_, anyhow::Error>(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn full_cleanup_debt_returns_retryable_503_until_reclaimed() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let auth = graph_auth(&fixture).await?;
        let admitted = preparation::admit(&fixture.state, fixture.library_id).await?;
        let pg = &fixture.state.persistence.postgres;
        let (row_id, mut cursor): (Uuid, serde_json::Value) = sqlx::query_as(
            "select id,cursor_json from maintenance_job_run where scope_id=$1 and class='graph.topology-preparation'",
        ).bind(fixture.library_id).fetch_one(pg).await?;
        cursor["records"][0]["status"] = serde_json::json!("failed");
        cursor["records"][0]["error_code"] = serde_json::json!("injected_failure");
        let due = (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        let debts: Vec<serde_json::Value> = (0..32).map(|_| {
            let attempt = Uuid::now_v7();
            serde_json::json!({
                "workspace_id": fixture.workspace_id,
                "library_id": fixture.library_id,
                "preparation_id": admitted.id,
                "attempt_id": attempt,
                "key": format!("graph-topology/{}/{}/{}/{}.ndjson.zst", fixture.workspace_id, fixture.library_id, admitted.id, attempt),
                "not_before": due,
                "cleanup": null,
            })
        }).collect();
        cursor["debts"] = serde_json::Value::Array(debts);
        sqlx::query("update maintenance_job_run set cursor_json=$2 where id=$1").bind(row_id).bind(cursor).execute(pg).await?;
        let error = create_graph_preparation(auth.clone(), State(fixture.state.clone()), Path(fixture.library_id)).await.err().context("full debt must reject admission")?;
        ensure!(matches!(&error, ironrag_backend::interfaces::http::router_support::ApiError::ServiceUnavailable { kind: "graph_preparation_capacity", .. }));
        ensure!(error.into_response().status() == axum::http::StatusCode::SERVICE_UNAVAILABLE);
        let count: i64 = sqlx::query_scalar("select jsonb_array_length(cursor_json->'debts')::bigint from maintenance_job_run where id=$1").bind(row_id).fetch_one(pg).await?;
        ensure!(count == 32);
        preparation::cleanup_due(&fixture.state).await?;
        preparation::cleanup_due(&fixture.state).await?;
        let count: i64 = sqlx::query_scalar("select jsonb_array_length(cursor_json->'debts')::bigint from maintenance_job_run where id=$1").bind(row_id).fetch_one(pg).await?;
        ensure!(count == 0);
        let response = create_graph_preparation(auth, State(fixture.state.clone()), Path(fixture.library_id)).await?;
        ensure!(response.status() == axum::http::StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 65536).await?;
        let successor: preparation::KnowledgeGraphPreparationResponse = serde_json::from_slice(&body)?;
        ensure!(successor.id != admitted.id && successor.status == PreparationStatus::Queued);
        Ok::<_, anyhow::Error>(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn all_preparation_routes_enforce_permissions_and_library_scope() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let auth = graph_auth(&fixture).await?;
        let admitted = preparation::admit(&fixture.state, fixture.library_id).await?;
        let unauthorized = scoped_workspace_auth(auth.principal_id, fixture.workspace_id, &[]);
        ensure!(
            create_graph_preparation(
                unauthorized.clone(),
                State(fixture.state.clone()),
                Path(fixture.library_id)
            )
            .await
            .is_err()
        );
        ensure!(
            get_graph_preparation(
                unauthorized.clone(),
                State(fixture.state.clone()),
                Path((fixture.library_id, admitted.id))
            )
            .await
            .is_err()
        );
        ensure!(
            Box::pin(get_graph_preparation_result(
                unauthorized,
                State(fixture.state.clone()),
                Path((fixture.library_id, admitted.id))
            ))
            .await
            .is_err()
        );
        let foreign_workspace = scoped_workspace_auth(
            auth.principal_id,
            Uuid::now_v7(),
            &["knowledge_read", "library_read"],
        );
        ensure!(
            create_graph_preparation(
                foreign_workspace.clone(),
                State(fixture.state.clone()),
                Path(fixture.library_id)
            )
            .await
            .is_err()
        );
        ensure!(
            get_graph_preparation(
                foreign_workspace.clone(),
                State(fixture.state.clone()),
                Path((fixture.library_id, admitted.id))
            )
            .await
            .is_err()
        );
        ensure!(
            Box::pin(get_graph_preparation_result(
                foreign_workspace,
                State(fixture.state.clone()),
                Path((fixture.library_id, admitted.id))
            ))
            .await
            .is_err()
        );
        let other = ironrag_backend::infra::repositories::catalog_repository::create_library(
            &fixture.state.persistence.postgres,
            fixture.workspace_id,
            "other-graph",
            "Other",
            None,
            None,
        )
        .await?;
        let wrong_status = get_graph_preparation(
            auth.clone(),
            State(fixture.state.clone()),
            Path((other.id, admitted.id)),
        )
        .await
        .err()
        .context("cross-library status must be denied")?;
        let wrong_result = Box::pin(get_graph_preparation_result(
            auth,
            State(fixture.state.clone()),
            Path((other.id, admitted.id)),
        ))
        .await
        .err()
        .context("cross-library result must be denied")?;
        ensure!(wrong_status.into_response().status() == axum::http::StatusCode::NOT_FOUND);
        ensure!(wrong_result.into_response().status() == axum::http::StatusCode::NOT_FOUND);
        Ok::<_, anyhow::Error>(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn http1_result_framing_completes_without_gzip() -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        seed_published_graph(&fixture).await?;
        let auth = graph_auth(&fixture).await?;
        let descriptor = prepare_graph(&fixture, auth.clone()).await?;
        let router = axum::Router::<ironrag_backend::app::state::AppState>::new().route(
            "/v1/knowledge/libraries/{library}/graph/preparations/{preparation}/result",
            axum::routing::get(move |State(state): State<ironrag_backend::app::state::AppState>, Path(ids): Path<(Uuid, Uuid)>| {
                let auth = auth.clone();
                async move { Box::pin(get_graph_preparation_result(auth, State(state), Path(ids))).await }
            }),
        );
        let router = ironrag_backend::interfaces::http::middleware::compression::apply(router).with_state(fixture.state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; }).await
        });
        let observed = async {
            let mut socket = tokio::net::TcpStream::connect(address).await?;
            let request = format!("GET /v1/knowledge/libraries/{}/graph/preparations/{}/result HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n", fixture.library_id, descriptor.id);
            socket.write_all(request.as_bytes()).await?;
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut bytes)).await??;
            let split = bytes.windows(4).position(|v| v == b"\r\n\r\n").context("HTTP headers")?;
            let headers = std::str::from_utf8(&bytes[..split])?.to_ascii_lowercase();
            ensure!(headers.starts_with("http/1.1 200 "));
            ensure!(headers.contains("content-encoding: identity"));
            ensure!(headers.contains("transfer-encoding: chunked"));
            let mut remaining = &bytes[split + 4..];
            let mut body = Vec::new();
            loop {
                let end = remaining.windows(2).position(|v| v == b"\r\n").context("chunk length")?;
                let size = usize::from_str_radix(std::str::from_utf8(&remaining[..end])?, 16)?;
                remaining = &remaining[end + 2..];
                if size == 0 { ensure!(remaining == b"\r\n", "terminal HTTP chunk incomplete"); break; }
                ensure!(remaining.len() >= size + 2 && &remaining[size..size + 2] == b"\r\n");
                body.extend_from_slice(&remaining[..size]);
                remaining = &remaining[size + 2..];
            }
            let last = body.split(|b| *b == b'\n').rfind(|line| !line.is_empty()).context("terminal graph frame")?;
            let end: serde_json::Value = serde_json::from_slice(last)?;
            ensure!(end["s"] == "end" && end["preparation_id"] == descriptor.id.to_string());
            Ok::<_, anyhow::Error>(())
        }.await;
        let _ = stop.send(());
        server.await??;
        observed
    }.await;
    fixture.cleanup().await?;
    result
}
