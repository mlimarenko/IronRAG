//! Retained graph payloads must be checked against the projection they loaded,
//! not a clean successor that happens to be current at the final read boundary.
#![cfg(feature = "test-support")]

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use axum::extract::{Path, State};
use ironrag_backend::{
    infra::repositories::{
        self,
        admission_repository::{
            self, ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
            VisibilityAdmissionRequest,
        },
        content_fingerprint_repository, iam_repository,
    },
    interfaces::http::{auth::AuthContext, knowledge::get_graph},
};
use ironrag_contracts::documents::{CreateVisibilityIntentRequest, ReplacementIdentity};
use redis::AsyncCommands;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::content_lifecycle_support::ContentLifecycleFixture;

const RESTRICTED_LABEL: &str = "restricted A graph label";
const RESTRICTED_SUMMARY: &str = "restricted A graph summary";

async fn graph_auth(fixture: &ContentLifecycleFixture) -> Result<AuthContext> {
    let principal = iam_repository::create_principal(
        &fixture.state.persistence.postgres,
        "api_token",
        "graph visibility race",
        None,
    )
    .await?;
    let permissions = ["knowledge_read", "library_read"];
    Ok(crate::iam_token_support::scoped_workspace_auth(
        principal.id,
        fixture.workspace_id,
        &permissions,
    ))
}

fn replacement(file_name: &str) -> ReplacementIdentity {
    ReplacementIdentity {
        file_name: file_name.to_owned(),
        checksum: format!("sha256:{}", hex::encode(Sha256::digest(b"frozen graph source A"))),
        mime_type: "text/plain".to_owned(),
        byte_size: 128,
        title: Some("graph visibility source".to_owned()),
        language_code: None,
        source_uri: None,
        document_hint: None,
    }
}

async fn seed_published_graph(
    fixture: &ContentLifecycleFixture,
) -> Result<(Uuid, Vec<Uuid>, Uuid)> {
    let pg = &fixture.state.persistence.postgres;
    let source = admission_repository::admit_content_with_failpoint(
        pg,
        &ContentAdmissionRequest {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            operation_kind: "upload".to_owned(),
            requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            idempotency_key: Some("graph-race-source".to_owned()),
            source_identity: Some("graph-race-source".to_owned()),
            target: ContentAdmissionTarget::New {
                external_key: Some("graph-race-source".to_owned()),
                file_name: Some("graph-source.txt".to_owned()),
                parent_external_key: None,
            },
            revision: Some(RevisionAdmission {
                content_source_kind: "upload".to_owned(),
                replacement: replacement("graph-source.txt"),
                visibility_intent_id: None,
                base_revision_id: None,
                storage_key: None,
            }),
            parent_async_operation_id: None,
            priority: 100,
        },
        None,
    )
    .await?
    .into_bundle();
    let document_id = source.document.id;
    let revision_id = source.revision.context("source revision")?.id;
    let item_id = source.item.context("source mutation item")?.id;
    sqlx::query("update content_mutation set mutation_state='applied' where id=$1")
        .bind(source.mutation.id)
        .execute(pg)
        .await?;
    sqlx::query("update content_mutation_item set item_state='applied' where id=$1")
        .bind(item_id)
        .execute(pg)
        .await?;
    sqlx::query(
        "update knowledge_revision set text_state='text_readable', text_readable_at=now(),
         vector_state='ready', vector_ready_at=now(), graph_state='ready', graph_ready_at=now()
         where revision_id=$1",
    )
    .bind(revision_id)
    .execute(pg)
    .await?;
    sqlx::query(
        "update content_document_head set active_revision_id=$2, readable_revision_id=$2 where document_id=$1",
    )
    .bind(document_id)
    .bind(revision_id)
    .execute(pg)
    .await?;
    ensure!(
        content_fingerprint_repository::get_current_revision_id(pg, document_id).await?
            == Some(revision_id),
        "frozen source A must be canonically published before graph selection"
    );

    let mut node_ids = Vec::new();
    for (key, label) in [("source", RESTRICTED_LABEL), ("target", "graph target")] {
        let node = repositories::upsert_runtime_graph_node(
            pg,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library_id,
                identity_key: key,
                label,
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: Some(RESTRICTED_SUMMARY),
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 1,
            },
        )
        .await?;
        node_ids.push(node.id);
    }
    let edge = repositories::upsert_runtime_graph_edge(
        pg,
        repositories::RuntimeGraphEdgeInput {
            library_id: fixture.library_id,
            from_node_id: node_ids[0],
            to_node_id: node_ids[1],
            relation_type: "depends_on",
            identity_key: "graph-race-edge",
            summary: Some(RESTRICTED_SUMMARY),
            weight: None,
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
        },
    )
    .await?;
    for (target_kind, target_id) in
        [("node", node_ids[0]), ("node", node_ids[1]), ("edge", edge.id)]
    {
        repositories::create_runtime_graph_evidence(
            pg,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: fixture.library_id,
                target_kind,
                target_id,
                document_id: Some(document_id),
                revision_id: Some(revision_id),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("graph-source.txt"),
                page_ref: None,
                evidence_text: RESTRICTED_SUMMARY,
                confidence_score: None,
                evidence_context_key: "frozen-source-a",
            },
        )
        .await?;
    }
    publish_snapshot(fixture, 1, "ready", 2, 1).await?;
    Ok((document_id, node_ids, edge.id))
}

async fn publish_snapshot(
    fixture: &ContentLifecycleFixture,
    version: i64,
    status: &str,
    nodes: i32,
    edges: i32,
) -> Result<()> {
    let epoch = Uuid::now_v7();
    for graph_status in ["building", status] {
        repositories::upsert_runtime_graph_snapshot(
            &fixture.state.persistence.postgres,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status,
                projection_version: version,
                build_epoch: epoch,
                node_count: nodes,
                edge_count: edges,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
            true,
        )
        .await?
        .context("frozen snapshot publication must own its claim")?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn http_cached_g1_is_denied_after_source_intent_and_clean_g2_publication() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let (document_id, node_ids, edge_id) = seed_published_graph(&fixture).await?;
        let auth = graph_auth(&fixture).await?;
        let library_id = fixture.library_id;
        let initial =
            Box::pin(get_graph(auth.clone(), State(fixture.state.clone()), Path(library_id)))
                .await?;
        let g1 = axum::body::to_bytes(initial.into_body(), 128 * 1024).await?;
        let text = std::str::from_utf8(&g1)?;
        ensure!(text.contains(RESTRICTED_LABEL) && text.contains(RESTRICTED_SUMMARY));
        let snapshot = repositories::get_runtime_graph_snapshot(pg, library_id)
            .await?
            .context("G1 snapshot")?;
        let selected_g1 = snapshot.projection_identity();
        ensure!(
            content_fingerprint_repository::graph_projection_is_current(pg, selected_g1).await?
        );
        let key = format!(
            "graph:{library_id}:v{}:g{}",
            snapshot.projection_version, snapshot.topology_generation
        );
        let mut redis = fixture.state.persistence.redis.get_multiplexed_async_connection().await?;
        let cached: Vec<u8> = redis.get(&key).await?;
        ensure!(cached == g1.as_ref(), "the actual HTTP handler must warm G1 bytes in Redis");

        let (reached, release) =
            fixture.state.runtime_graph_projection_cache.pause_next_graph_read(library_id).await;
        let state = fixture.state.clone();
        let reader =
            tokio::spawn(
                async move { Box::pin(get_graph(auth, State(state), Path(library_id))).await },
            );
        let mut reader = ironrag_backend::shared::cancellation::AbortOnDrop::new(reader);
        tokio::time::timeout(Duration::from_secs(10), reached)
            .await
            .context("HTTP handler never retained G1 before its final graph read check")??;

        // The actual handler is parked with old G1 bytes, not merely between
        // two membership observations made by the test itself.
        admission_repository::admit_visibility_intent_with_failpoint(
            pg,
            &VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id,
                library_id,
                document_id,
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "graph-race-withdraw-a".to_owned(),
                    source_identity: "graph-race-source".to_owned(),
                    materialization_token: None,
                    replacement: replacement("replacement.txt"),
                },
            },
            None,
        )
        .await?;
        ensure!(
            content_fingerprint_repository::get_current_revision_id(pg, document_id)
                .await?
                .is_none()
        );
        ensure!(
            !content_fingerprint_repository::graph_projection_is_current(pg, selected_g1).await?,
            "unchanged G1 identity cannot authorize its withdrawn source"
        );
        publish_snapshot(&fixture, 2, "empty", 0, 0).await?;
        repositories::prune_runtime_graph_projection_rows(pg, library_id, 1, &node_ids, &[edge_id])
            .await?;
        let remaining: i64 =
            sqlx::query_scalar("select count(*) from runtime_graph_evidence where library_id=$1")
                .bind(library_id)
                .fetch_one(pg)
                .await?;
        ensure!(remaining == 0, "G2 GC must remove the evidence that could expose old membership");
        let selected_g2 = repositories::get_runtime_graph_snapshot(pg, library_id)
            .await?
            .context("G2 snapshot")?
            .projection_identity();
        ensure!(
            content_fingerprint_repository::graph_projection_is_current(pg, selected_g2).await?
        );
        let wrong_generation = repositories::RuntimeGraphProjectionIdentity {
            topology_generation: selected_g2.topology_generation - 1,
            ..selected_g2
        };
        ensure!(
            !content_fingerprint_repository::graph_projection_is_current(pg, wrong_generation)
                .await?,
            "the selected generation, not only its projection version, must still be current"
        );
        release.send(()).map_err(|()| anyhow::anyhow!("parked HTTP graph reader disappeared"))?;
        let returned = tokio::time::timeout(
            Duration::from_secs(10),
            reader.handle_mut().context("HTTP graph reader handle")?,
        )
        .await
        .context("HTTP graph reader did not finish after release")??;
        let denied = returned.is_err();
        let leaked = match returned {
            Err(_) => false,
            Ok(response) => {
                let body = axum::body::to_bytes(response.into_body(), 128 * 1024).await?;
                std::str::from_utf8(&body)?.contains(RESTRICTED_LABEL)
            }
        };
        let _: usize = redis.del(&key).await?;
        // Assert after collecting the response, so this is a behavioural RED
        // against the unmodified read decision, not a source-pattern assertion.
        ensure!(
            !leaked,
            "selected G1 restricted label leaked after A intent and clean G2 publication"
        );
        ensure!(
            denied,
            "a superseded selection must be denied, not replaced with a different payload"
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}
