use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Instant, sleep, timeout};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::ai::AiBindingPurpose,
    infra::repositories::{self, ai_repository, iam_repository},
    infra::{
        knowledge_plane::{
            CanonicalIngestVectorWriteFence, CanonicalVectorWriteFence, DocumentStore, GraphStore,
            SearchStore, VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX,
            VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
        },
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkRow,
            KnowledgeChunkVectorRow, KnowledgeDocumentRow, KnowledgeEntityVectorRow,
            KnowledgeRevisionRow, KnowledgeTechnicalFactRow, NewKnowledgeEntity,
            NewKnowledgeEvidence,
        },
        postgres::{
            pg_document_store::PgDocumentStore, pg_graph_store::PgGraphStore,
            pg_search_store::PgSearchStore,
        },
    },
    integrations::llm::{EmbeddingRequest, EmbeddingResponse, LlmGateway},
    interfaces::http::{auth::hash_token, authorization::PERMISSION_LIBRARY_READ, router},
    services::query::search::SearchService,
    shared::secret_encryption::SecretPurpose,
};

const SEARCH_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

fn synthetic_embedding_profile_key(seed: Uuid) -> String {
    format!("embedding-profile:v1:{0}{0}", seed.simple())
}

async fn create_empty_vector_test_library(
    fixture: &KnowledgeSearchFixture,
    label: &str,
) -> Result<(Uuid, Uuid)> {
    let suffix = Uuid::now_v7().simple().to_string();
    let workspace = repositories::catalog_repository::create_workspace(
        &fixture.postgres,
        &format!("{label}-workspace-{suffix}"),
        "Vector Fence Workspace",
        None,
    )
    .await?;
    let library = repositories::catalog_repository::create_library(
        &fixture.postgres,
        workspace.id,
        &format!("{label}-library-{suffix}"),
        "Vector Fence Library",
        None,
        None,
    )
    .await?;
    Ok((workspace.id, library.id))
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn exact_profile_dimension_claim_is_durable_unique_and_ignores_legacy_keys() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let library_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let empty_profile = synthetic_embedding_profile_key(Uuid::now_v7());
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 3, $2, $3, 'knowledge_chunk_vector_d3', true, 0, false)",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&empty_profile)
        .execute(&fixture.postgres)
        .await?;
        assert_eq!(
            fixture
                .search_store
                .read_vector_profile_dimension_claim(
                    library_id,
                    &empty_profile,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                )
                .await?,
            Some(3),
        );

        let legacy_key = Uuid::now_v7().to_string();
        for dimensions in [3, 4] {
            sqlx::query(
                "insert into knowledge_vector_relation_manifest (
                    library_id, dim, vector_kind, embedding_model_key, relation_name,
                    is_default, row_count, promoted
                 ) values ($1, $2, $3, $4, $5, false, 0, false)",
            )
            .bind(library_id)
            .bind(dimensions)
            .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
            .bind(&legacy_key)
            .bind(format!("knowledge_chunk_vector_d{dimensions}"))
            .execute(&fixture.postgres)
            .await?;
        }
        assert!(
            fixture
                .search_store
                .read_vector_profile_dimension_claim(
                    library_id,
                    &legacy_key,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                )
                .await
                .is_err(),
            "legacy UUID manifests must never become exact-profile claims",
        );
        let missing_exact_profile = synthetic_embedding_profile_key(Uuid::now_v7());
        assert_eq!(
            fixture
                .search_store
                .read_vector_profile_dimension_claim(
                    library_id,
                    &missing_exact_profile,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                )
                .await?,
            None,
            "an unrelated legacy manifest must not become an exact-profile fallback",
        );
        let opaque_key = format!("opaque-test-key-{}", Uuid::now_v7());
        for dimensions in [3, 4] {
            sqlx::query(
                "insert into knowledge_vector_relation_manifest (
                    library_id, dim, vector_kind, embedding_model_key, relation_name,
                    is_default, row_count, promoted
                 ) values ($1, $2, $3, $4, $5, false, 0, false)",
            )
            .bind(library_id)
            .bind(dimensions)
            .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
            .bind(&opaque_key)
            .bind(format!("knowledge_chunk_vector_d{dimensions}"))
            .execute(&fixture.postgres)
            .await?;
        }

        let rebuild_profile =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 3, $2, $3, 'knowledge_chunk_vector_d3', false, 0, false)",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&rebuild_profile)
        .execute(&fixture.postgres)
        .await?;
        let rebuild_conflict = sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 4, $2, $3, 'knowledge_chunk_vector_d4', false, 0, false)",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&rebuild_profile)
        .execute(&fixture.postgres)
        .await
        .unwrap_err();
        assert_eq!(
            rebuild_conflict.as_database_error().and_then(|error| error.code()).as_deref(),
            Some("23505")
        );

        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let first_row = KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            embedding_model_key: profile.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.1, 0.2, 0.3],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        };
        fixture.search_store.upsert_chunk_vector(&first_row).await?;
        let mut same_dimension_row = first_row.clone();
        same_dimension_row.vector_id = Uuid::now_v7();
        same_dimension_row.chunk_id = Uuid::now_v7();
        fixture.search_store.upsert_chunk_vector(&same_dimension_row).await?;

        let mut conflicting_row = first_row.clone();
        conflicting_row.vector_id = Uuid::now_v7();
        conflicting_row.chunk_id = Uuid::now_v7();
        conflicting_row.dimensions = 4;
        conflicting_row.vector = vec![0.1, 0.2, 0.3, 0.4];
        let conflict =
            fixture.search_store.upsert_chunk_vector(&conflicting_row).await.unwrap_err();
        let database_error = conflict
            .chain()
            .find_map(|error| error.downcast_ref::<sqlx::Error>())
            .and_then(|error| error.as_database_error())
            .and_then(|error| error.code());
        assert_eq!(database_error.as_deref(), Some("23505"));
        let losing_row_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d4
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(losing_row_count, 0);

        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 3, $2, $3, 'knowledge_entity_vector_d3', false, 0, false)",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .bind(&profile)
        .execute(&fixture.postgres)
        .await?;
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 3, $2, $3, 'knowledge_chunk_vector_d3', false, 0, false)",
        )
        .bind(Uuid::now_v7())
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&profile)
        .execute(&fixture.postgres)
        .await?;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn empty_library_vector_purge_removes_stale_rows_and_manifests_without_a_dimension()
-> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("empty-vector-workspace-{suffix}"),
            "Empty Vector Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("empty-vector-library-{suffix}"),
            "Empty Vector Library",
            None,
            None,
        )
        .await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                embedding_model_key: profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id: Uuid::now_v7(),
                embedding_model_key: profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.3, 0.2, 0.1],
                freshness_generation: 1,
                created_at: Utc::now(),
            })
            .await?;
        let source_version = repositories::catalog_repository::get_library_source_truth_version(
            &fixture.postgres,
            library.id,
        )
        .await?;
        assert_eq!(
            fixture
                .search_store
                .purge_empty_library_vector_plane(library.id, source_version)
                .await?,
            2
        );
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(library.id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(manifest_count, 0);
        assert_eq!(
            fixture
                .search_store
                .read_vector_profile_dimension_claim(
                    library.id,
                    &profile,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                )
                .await?,
            None
        );
        assert!(
            fixture
                .search_store
                .purge_empty_library_vector_plane(library.id, source_version)
                .await
                .is_err(),
            "a stale source fence must not authorize a second purge",
        );
        let current_source_version =
            repositories::catalog_repository::get_library_source_truth_version(
                &fixture.postgres,
                library.id,
            )
            .await?;
        assert_eq!(
            fixture
                .search_store
                .purge_empty_library_vector_plane(library.id, current_source_version)
                .await?,
            0
        );
        assert_eq!(
            repositories::catalog_repository::get_library_source_truth_version(
                &fixture.postgres,
                library.id,
            )
            .await?,
            current_source_version,
            "a repeated no-op purge must not advance the source fence",
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn fenced_vector_write_rolls_back_manifest_and_rows_after_source_drift() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "vector-fence-rollback").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let stale_source_version =
            repositories::catalog_repository::get_library_source_truth_version(
                &fixture.postgres,
                library_id,
            )
            .await?;
        repositories::catalog_repository::touch_library_source_truth_version(
            &fixture.postgres,
            library_id,
        )
        .await?;
        let row = KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            embedding_model_key: profile.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.1, 0.2, 0.3],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        };
        let error = fixture
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                std::slice::from_ref(&row),
                &CanonicalVectorWriteFence {
                    expected_source_truth_version: stale_source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: None,
                    advance_source_truth_version: false,
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("source or embedding profile changed"));
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        let vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!((manifest_count, vector_count), (0, 0));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn empty_purge_wins_before_queued_cross_replica_ingest_write() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "vector-purge-race").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                embedding_model_key: profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        let source_version = repositories::catalog_repository::get_library_source_truth_version(
            &fixture.postgres,
            library_id,
        )
        .await?;

        // Hold the shared side long enough to put the real purge behind the
        // exclusive side of the exact same advisory key.
        let mut shared_blocker = fixture.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock_shared(hashtextextended($1::text, 0))")
            .bind(format!("{VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX}:{library_id}"))
            .execute(&mut *shared_blocker)
            .await?;
        let purge_store = fixture.search_store.clone();
        let mut purge_task = tokio::spawn(async move {
            purge_store.purge_empty_library_vector_plane(library_id, source_version).await
        });

        let mut exclusive_waiter_observed = false;
        for _ in 0..40 {
            let waiters = sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_locks
                 where locktype = 'advisory'
                   and database = (select oid from pg_database where datname = current_database())
                   and not granted",
            )
            .fetch_one(&fixture.postgres)
            .await?;
            if waiters > 0 {
                exclusive_waiter_observed = true;
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert!(exclusive_waiter_observed, "purge did not wait on the shared data lock");

        let queued_row = KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            embedding_model_key: profile.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.3, 0.2, 0.1],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        };
        let writer_store = fixture.search_store.clone();
        let writer_profile = profile.clone();
        let mut writer_task = tokio::spawn(async move {
            writer_store
                .upsert_chunk_vectors_bulk_fenced(
                    std::slice::from_ref(&queued_row),
                    &CanonicalVectorWriteFence {
                        expected_source_truth_version: source_version,
                        embedding_profile_key: writer_profile,
                        ingest_attempt: None,
                        advance_source_truth_version: false,
                    },
                )
                .await
        });
        assert!(
            timeout(Duration::from_millis(250), &mut writer_task).await.is_err(),
            "queued ingest write bypassed the pending exclusive purge"
        );
        shared_blocker.rollback().await?;

        let purged = timeout(Duration::from_secs(5), &mut purge_task)
            .await
            .context("purge remained blocked after shared lock release")??
            .context("purge task failed")?;
        assert_eq!(purged, 1);
        let writer_result = timeout(Duration::from_secs(5), &mut writer_task)
            .await
            .context("ingest write remained blocked after purge commit")??;
        assert!(writer_result.is_err(), "stale queued writer must fail its source/profile fence");

        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.postgres)
        .await?;
        let vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!((manifest_count, vector_count), (0, 0));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn attempt_owned_cleanup_deletes_only_exact_vector_ids_and_reconciles_manifest() -> Result<()>
{
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "attempt-owned-vector-cleanup").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let target_revision_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();
        let pre_existing_id = Uuid::now_v7();
        let attempt_created_id = Uuid::now_v7();
        let concurrent_retry_id = Uuid::now_v7();
        let other_revision_vector_id = Uuid::now_v7();
        let rows = [
            (pre_existing_id, target_revision_id),
            (attempt_created_id, target_revision_id),
            (concurrent_retry_id, target_revision_id),
            (other_revision_vector_id, other_revision_id),
        ]
        .into_iter()
        .map(|(vector_id, revision_id)| KnowledgeChunkVectorRow {
            vector_id,
            workspace_id,
            library_id,
            chunk_id: Uuid::now_v7(),
            revision_id,
            embedding_model_key: profile.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.1, 0.2, 0.3],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        })
        .collect::<Vec<_>>();
        fixture.search_store.upsert_chunk_vectors_bulk(&rows).await?;

        let source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library_id).await?;
        let cleanup = fixture
            .search_store
            .delete_chunk_vectors_by_ids_fenced(library_id, &[attempt_created_id], source_version)
            .await?;
        assert_eq!(cleanup.deleted, 1);
        assert!(cleanup.source_truth_version > source_version);

        let remaining_ids = sqlx::query_scalar::<_, Uuid>(
            "select vector_id
             from knowledge_chunk_vector_d3
             where library_id = $1 and embedding_model_key = $2
             order by vector_id",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_all(&fixture.postgres)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(
            remaining_ids,
            BTreeSet::from([pre_existing_id, concurrent_retry_id, other_revision_vector_id,]),
            "terminal cleanup must preserve pre-existing and replacement-attempt vectors",
        );
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and dim = 3
               and vector_kind = $2
               and embedding_model_key = $3",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(manifest_count, 3);

        assert!(
            fixture
                .search_store
                .delete_chunk_vectors_by_ids_fenced(
                    library_id,
                    &[concurrent_retry_id],
                    source_version,
                )
                .await
                .is_err(),
            "a stale attempt source fence must not authorize cleanup",
        );

        let revision_delete = fixture
            .search_store
            .delete_chunk_vectors_by_revision_fenced(
                library_id,
                target_revision_id,
                cleanup.source_truth_version,
            )
            .await?;
        assert_eq!(revision_delete.deleted, 2);
        assert!(revision_delete.source_truth_version > cleanup.source_truth_version);
        let surviving_ids = sqlx::query_scalar::<_, Uuid>(
            "select vector_id
             from knowledge_chunk_vector_d3
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_all(&fixture.postgres)
        .await?;
        assert_eq!(surviving_ids, [other_revision_vector_id]);
        let reconciled_count = sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and dim = 3
               and vector_kind = $2
               and embedding_model_key = $3",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(reconciled_count, 1);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn latest_ingest_attempt_replaces_one_logical_vector_and_fences_stale_writer() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "logical-vector-overlap").await?;
        let revision_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let old_attempt_id = Uuid::now_v7();
        let new_attempt_id = Uuid::now_v7();
        sqlx::query(
            "insert into ingest_job (
                id, workspace_id, library_id, knowledge_revision_id,
                job_kind, queue_state, queue_leased_at,
                queue_lease_token, queue_lease_owner
             ) values (
                $1, $2, $3, $4,
                'content_mutation', 'leased', now(), $5, $6
             )",
        )
        .bind(job_id)
        .bind(workspace_id)
        .bind(library_id)
        .bind(revision_id)
        .bind(format!("logical-vector-overlap-{job_id}"))
        .bind("logical-vector-overlap-test")
        .execute(&fixture.postgres)
        .await?;
        sqlx::query(
            "insert into ingest_attempt (
                id, job_id, attempt_number, attempt_state,
                lease_token, heartbeat_at
             ) values ($1, $2, 1, 'leased', $3, now())",
        )
        .bind(old_attempt_id)
        .bind(job_id)
        .bind(format!("old-{old_attempt_id}"))
        .execute(&fixture.postgres)
        .await?;

        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let chunk_id = Uuid::now_v7();
        let old_vector_id = Uuid::now_v7();
        let source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library_id).await?;
        let old_row = KnowledgeChunkVectorRow {
            vector_id: old_vector_id,
            workspace_id,
            library_id,
            chunk_id,
            revision_id,
            embedding_model_key: profile.clone(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: vec![0.1, 0.2, 0.3],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        };
        fixture
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                std::slice::from_ref(&old_row),
                &CanonicalVectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(CanonicalIngestVectorWriteFence {
                        attempt_id: old_attempt_id,
                        revision_id,
                    }),
                    advance_source_truth_version: false,
                },
            )
            .await?;

        sqlx::query(
            "insert into ingest_attempt (
                id, job_id, attempt_number, attempt_state,
                lease_token, heartbeat_at
             ) values ($1, $2, 2, 'leased', $3, now())",
        )
        .bind(new_attempt_id)
        .bind(job_id)
        .bind(format!("new-{new_attempt_id}"))
        .execute(&fixture.postgres)
        .await?;

        let mut stale_row = old_row.clone();
        stale_row.vector_id = Uuid::now_v7();
        stale_row.vector = vec![0.9, 0.0, 0.1];
        let stale_result = fixture
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                std::slice::from_ref(&stale_row),
                &CanonicalVectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(CanonicalIngestVectorWriteFence {
                        attempt_id: old_attempt_id,
                        revision_id,
                    }),
                    advance_source_truth_version: false,
                },
            )
            .await;
        assert!(
            stale_result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("attempt authority changed")),
            "an older still-leased attempt must lose write authority after handoff",
        );

        // The retry may adopt already-complete coverage without rewriting the
        // physical UUID. Cleanup from the former owner must therefore be
        // authority-fenced, not merely exact-ID-fenced.
        let preserved_cleanup = fixture
            .search_store
            .delete_attempt_owned_chunk_vectors_by_ids_fenced(
                library_id,
                &[old_vector_id],
                source_version,
                CanonicalIngestVectorWriteFence { attempt_id: old_attempt_id, revision_id },
            )
            .await?;
        assert!(
            preserved_cleanup.is_none(),
            "cleanup must preserve an adopted UUID after attempt authority moves",
        );
        let adopted_vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and vector_id = $2",
        )
        .bind(library_id)
        .bind(old_vector_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(adopted_vector_count, 1);

        let new_vector_id = Uuid::now_v7();
        let mut new_row = old_row.clone();
        new_row.vector_id = new_vector_id;
        new_row.vector = vec![0.3, 0.2, 0.1];
        new_row.created_at = Utc::now();
        fixture
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                std::slice::from_ref(&new_row),
                &CanonicalVectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(CanonicalIngestVectorWriteFence {
                        attempt_id: new_attempt_id,
                        revision_id,
                    }),
                    advance_source_truth_version: false,
                },
            )
            .await?;

        let authoritative_ids = sqlx::query_scalar::<_, Uuid>(
            "select vector_id
             from knowledge_chunk_vector_d3
             where library_id = $1
               and chunk_id = $2
               and revision_id = $3
               and embedding_model_key = $4
               and vector_kind = $5
               and freshness_generation = 1",
        )
        .bind(library_id)
        .bind(chunk_id)
        .bind(revision_id)
        .bind(&profile)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .fetch_all(&fixture.postgres)
        .await?;
        assert_eq!(authoritative_ids, [new_vector_id]);

        let cleanup = fixture
            .search_store
            .delete_chunk_vectors_by_ids_fenced(library_id, &[old_vector_id], source_version)
            .await?;
        assert_eq!(cleanup.deleted, 0);
        assert_eq!(cleanup.source_truth_version, source_version);
        let authoritative_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and vector_id = $2",
        )
        .bind(library_id)
        .bind(new_vector_id)
        .fetch_one(&fixture.postgres)
        .await?;
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and dim = 3
               and vector_kind = $2
               and embedding_model_key = $3",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!((authoritative_count, manifest_count), (1, 1));

        let cleanup_control_vector_id = Uuid::now_v7();
        let mut cleanup_control_row = new_row;
        cleanup_control_row.vector_id = cleanup_control_vector_id;
        cleanup_control_row.chunk_id = Uuid::now_v7();
        fixture
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                std::slice::from_ref(&cleanup_control_row),
                &CanonicalVectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile,
                    ingest_attempt: Some(CanonicalIngestVectorWriteFence {
                        attempt_id: new_attempt_id,
                        revision_id,
                    }),
                    advance_source_truth_version: false,
                },
            )
            .await?;
        let authorized_cleanup = fixture
            .search_store
            .delete_attempt_owned_chunk_vectors_by_ids_fenced(
                library_id,
                &[cleanup_control_vector_id],
                source_version,
                CanonicalIngestVectorWriteFence { attempt_id: new_attempt_id, revision_id },
            )
            .await?
            .context("latest leased attempt must retain cleanup authority")?;
        assert_eq!(authorized_cleanup.deleted, 1);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn logical_vector_index_upgrade_aborts_without_deleting_existing_duplicates() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "logical-index-upgrade").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let original_vector_id = Uuid::now_v7();
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: original_vector_id,
                workspace_id,
                library_id,
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                embedding_model_key: profile,
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        sqlx::query("drop index knowledge_chunk_vector_d3_logical_key")
            .execute(&fixture.postgres)
            .await?;
        let duplicate_vector_id = Uuid::now_v7();
        sqlx::query(
            "insert into knowledge_chunk_vector_d3 (
                key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding,
                freshness_generation, created_at, occurred_at, occurred_until
             )
             select $2::text, $2, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, now(), occurred_at, occurred_until
             from knowledge_chunk_vector_d3
             where vector_id = $1",
        )
        .bind(original_vector_id)
        .bind(duplicate_vector_id)
        .execute(&fixture.postgres)
        .await?;

        let migration_error =
            sqlx::raw_sql(include_str!("../migrations/0007_safe_catalog_defaults.sql"))
                .execute(&fixture.postgres)
                .await
                .expect_err("upgrade must stop for ambiguous duplicate provenance");
        assert!(migration_error.to_string().contains("require explicit repair"));
        let duplicate_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(duplicate_count, 2, "failed upgrade must not discard either row");

        sqlx::query("delete from knowledge_chunk_vector_d3 where vector_id = $1")
            .bind(duplicate_vector_id)
            .execute(&fixture.postgres)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/0007_safe_catalog_defaults.sql"))
            .execute(&fixture.postgres)
            .await?;
        let index_exists = sqlx::query_scalar::<_, bool>(
            "select to_regclass(
                format('%I.%I', current_schema(), 'knowledge_chunk_vector_d3_logical_key')
             ) is not null",
        )
        .fetch_one(&fixture.postgres)
        .await?;
        assert!(index_exists);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn entity_library_fenced_delete_is_atomic_and_reconciles_manifest() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "entity-vector-delete").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        for vector in [vec![0.1, 0.2, 0.3], vec![0.3, 0.2, 0.1]] {
            fixture
                .search_store
                .upsert_entity_vector(&KnowledgeEntityVectorRow {
                    vector_id: Uuid::now_v7(),
                    workspace_id,
                    library_id,
                    entity_id: Uuid::now_v7(),
                    embedding_model_key: profile.clone(),
                    vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                    dimensions: 3,
                    vector,
                    freshness_generation: 1,
                    created_at: Utc::now(),
                })
                .await?;
        }
        let source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library_id).await?;
        let outcome = fixture
            .search_store
            .delete_entity_vectors_by_library_fenced(library_id, source_version)
            .await?;
        assert_eq!(outcome.deleted, 2);
        assert!(outcome.source_truth_version > source_version);
        let vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.postgres)
        .await?;
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and dim = 3
               and vector_kind = $2
               and embedding_model_key = $3",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!((vector_count, manifest_count), (0, 0));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

fn test_credential_master_key() -> String {
    STANDARD.encode([31_u8; 32])
}

struct KnowledgeSearchFixture {
    temp_database: TempPostgresDatabase,
    postgres: PgPool,
    document_store: PgDocumentStore,
    graph_store: PgGraphStore,
    search_store: PgSearchStore,
}

impl KnowledgeSearchFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for knowledge search tests")?;
        let temp_database = TempPostgresDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect knowledge search postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply knowledge search migrations")?;

        Ok(Self {
            temp_database,
            postgres: postgres.clone(),
            document_store: PgDocumentStore { pool: postgres.clone() },
            graph_store: PgGraphStore { pool: postgres.clone() },
            search_store: PgSearchStore { pool: postgres.clone() },
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.postgres.close().await;
        self.temp_database.drop().await
    }

    async fn wait_for_chunk_hits(
        &self,
        library_id: Uuid,
        query: &str,
        expected_chunk_ids: &[Uuid],
    ) -> Result<Vec<Uuid>> {
        let expected = expected_chunk_ids.iter().copied().collect::<BTreeSet<_>>();
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = self
                .search_store
                .search_chunks(library_id, query, expected_chunk_ids.len().max(8), None, None)
                .await
                .with_context(|| format!("failed to search chunks for query {query}"))?;
            let actual = hits.iter().map(|row| row.chunk_id).collect::<BTreeSet<_>>();
            if actual == expected {
                return Ok(hits.into_iter().map(|row| row.chunk_id).collect());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for chunk hits {expected:?} for query {query}; last observed {actual:?}"
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
}

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("knowledge_search_http_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {name}"))?;
        admin_pool.close().await;

        Ok(Self { database_url: replace_database_name(base_database_url, &name)?, admin_url, name })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect postgres admin database for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeEmbeddingGateway {
    embedding: Vec<f32>,
}

#[async_trait]
impl LlmGateway for FakeEmbeddingGateway {
    async fn generate(
        &self,
        request: ironrag_backend::integrations::llm::ChatRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::ChatResponse> {
        Err(anyhow!("generate not used in knowledge search test: {}", request.provider_kind))
    }

    async fn embed(&self, mut request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Ok(EmbeddingResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: self.embedding.len(),
            embedding: self.embedding.clone(),
            usage_json: json!({}),
        })
    }

    async fn embed_many(
        &self,
        mut request: ironrag_backend::integrations::llm::EmbeddingBatchRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::EmbeddingBatchResponse> {
        let embeddings = std::mem::take(&mut request.inputs)
            .into_iter()
            .map(|_| self.embedding.clone())
            .collect::<Vec<_>>();
        Ok(ironrag_backend::integrations::llm::EmbeddingBatchResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: self.embedding.len(),
            embeddings,
            usage_json: json!({}),
        })
    }

    async fn vision_extract(
        &self,
        request: ironrag_backend::integrations::llm::VisionRequest,
    ) -> anyhow::Result<ironrag_backend::integrations::llm::VisionResponse> {
        Err(anyhow!("vision_extract not used in knowledge search test: {}", request.provider_kind))
    }
}

struct KnowledgeSearchHttpFixture {
    temp_postgres: TempPostgresDatabase,
    state: AppState,
    token: String,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    fact_id: Uuid,
    entity_id: Uuid,
    relation_id: Uuid,
}

impl KnowledgeSearchHttpFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env()
            .context("failed to load settings for knowledge search http test")?;
        let temp_postgres = TempPostgresDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_postgres.database_url.clone();
        settings.credential_master_key = Some(test_credential_master_key());
        settings.credential_encryption_write_enabled = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect to knowledge search postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply knowledge search migrations")?;
        postgres.close().await;

        let mut state = AppState::new(settings).await?;
        state.llm_gateway = Arc::new(FakeEmbeddingGateway { embedding: vec![0.9, 0.8, 0.7] });

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("knowledge-search-workspace-{suffix}"),
            "Knowledge Search Workspace",
            None,
        )
        .await
        .context("failed to create knowledge search workspace")?;
        let library = repositories::catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("knowledge-search-library-{suffix}"),
            "Knowledge Search Library",
            Some("knowledge search proof fixture"),
            None,
        )
        .await
        .context("failed to create knowledge search library")?;

        let provider_catalog = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .context("failed to list provider catalog for knowledge search test")?
            .into_iter()
            .find(|row| row.provider_kind == "openai")
            .context("expected seeded openai provider catalog row")?;
        let model_catalog = ai_repository::list_model_catalog(
            &state.persistence.postgres,
            Some(provider_catalog.id),
        )
        .await
        .context("failed to list model catalog for knowledge search test")?
        .into_iter()
        .find(|row| row.capability_kind == "embedding")
        .context("expected seeded embedding model catalog row")?;
        let account_id = Uuid::now_v7();
        let encrypted_api_key = state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            "secret://knowledge-search/provider",
        )?;
        let credential = ai_repository::create_account(
            &state.persistence.postgres,
            account_id,
            "workspace",
            Some(workspace.id),
            None,
            provider_catalog.id,
            "knowledge-search-provider-credential",
            Some(&encrypted_api_key),
            None,
            None,
        )
        .await
        .context("failed to create knowledge search AI account")?;
        ai_repository::create_binding(
            &state.persistence.postgres,
            ai_repository::CreateAiBindingInput {
                scope_kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
                binding_purpose: "embed_chunk",
                account_id: credential.id,
                model_catalog_id: model_catalog.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: json!({}),
                updated_by_principal_id: None,
            },
        )
        .await
        .context("failed to create knowledge search library binding")?;
        let embedding_profile_key = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(&state, library.id, AiBindingPurpose::EmbedChunk)
            .await
            .context("failed to resolve knowledge search embedding binding")?
            .context("knowledge search embedding binding was not resolved")?
            .embedding_execution_profile_key();

        let token =
            mint_library_read_token(&state.persistence.postgres, workspace.id, library.id).await?;

        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let fact_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let relation_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        let now = Utc::now();

        state
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: "search-document".to_string(),
                file_name: None,
                title: Some("Search Document".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert knowledge search document")?;
        state
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://knowledge-search".to_string()),
                source_uri: Some("memory://knowledge-search/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "knowledge-search-checksum".to_string(),
                title: Some("Knowledge Search".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("knowledge-search-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "ready".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: Some(now),
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert knowledge search revision")?;
        state
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert knowledge search chunk")?;
        state
            .document_store
            .replace_technical_facts(
                revision_id,
                &[KnowledgeTechnicalFactRow {
                    fact_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    document_id,
                    revision_id,
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value_text: "/orion/status".to_string(),
                    canonical_value_exact: "/orion/status".to_string(),
                    canonical_value_json: json!({
                        "value_type": "text",
                        "value": "/orion/status"
                    }),
                    display_value: "/orion/status".to_string(),
                    qualifiers_json: json!([]),
                    support_block_ids: Vec::new(),
                    support_chunk_ids: vec![chunk_id],
                    confidence: Some(0.98),
                    extraction_kind: "fixture_seed".to_string(),
                    conflict_group_id: None,
                    created_at: now,
                    updated_at: now,
                }],
            )
            .await
            .context("failed to insert knowledge search technical fact")?;
        state
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id: workspace.id,
                library_id: library.id,
                canonical_label: "Orion Signal".to_string(),
                aliases: vec!["Signal Orion".to_string()],
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: Some("Orion entity summary".to_string()),
                confidence: Some(0.95),
                support_count: 3,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(now),
                updated_at: Some(now),
            })
            .await
            .context("failed to insert knowledge search entity")?;
        state
            .graph_store
            .upsert_relation_with_endpoints(
                &ironrag_backend::infra::knowledge_rows::NewKnowledgeRelation {
                    relation_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    predicate: "Orion relation".to_string(),
                    normalized_assertion: "orion relation".to_string(),
                    confidence: Some(0.9),
                    support_count: 2,
                    contradiction_state: "none".to_string(),
                    freshness_generation: 1,
                    relation_state: "active".to_string(),
                    created_at: Some(now),
                    updated_at: Some(now),
                },
                Some(entity_id),
                Some(entity_id),
                library.id,
            )
            .await
            .context("failed to insert knowledge search relation")?;
        state
            .graph_store
            .upsert_relation_subject_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation subject")?;
        state
            .graph_store
            .upsert_relation_object_edge(relation_id, entity_id, library.id)
            .await
            .context("failed to link knowledge search relation object")?;
        state
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id,
                revision_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert knowledge search chunk vector")?;
        state
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 1,
                created_at: now,
            })
            .await
            .context("failed to insert knowledge search entity vector")?;
        state
            .graph_store
            .upsert_evidence_with_edges(
                &NewKnowledgeEvidence {
                    evidence_id,
                    workspace_id: workspace.id,
                    library_id: library.id,
                    document_id,
                    revision_id,
                    chunk_id: Some(chunk_id),
                    block_id: None,
                    fact_id: Some(fact_id),
                    span_start: Some(0),
                    span_end: Some(20),
                    quote_text: "orion lexical anchor".to_string(),
                    literal_spans_json: serde_json::json!([]),
                    evidence_kind: "chunk_quote".to_string(),
                    extraction_method: "seed".to_string(),
                    confidence: Some(0.99),
                    evidence_state: "active".to_string(),
                    freshness_generation: 1,
                    created_at: Some(now),
                    updated_at: Some(now),
                },
                Some(revision_id),
                Some(entity_id),
                Some(relation_id),
                None,
                library.id,
            )
            .await
            .context("failed to insert knowledge search evidence")?;

        Ok(Self {
            temp_postgres,
            state,
            token,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            chunk_id,
            fact_id,
            entity_id,
            relation_id,
        })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await
    }

    async fn search_document_hit(&self, query: &str) -> Result<Value> {
        let request_body = serde_json::to_vec(&json!({
            "text": query,
            "limit": 5,
            "chunkHitLimitPerDocument": 3,
            "evidenceSampleLimit": 2,
        }))
        .context("failed to encode knowledge search request body")?;
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/knowledge/libraries/{}/search", self.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(request_body))
                    .context("failed to build knowledge search request")?,
            )
            .await
            .context("failed to call knowledge search endpoint")?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .context("failed to read knowledge search response body")?
            .to_bytes();
        serde_json::from_slice::<Value>(&body).context("failed to decode knowledge search json")
    }

    async fn wait_for_query_evidence_top_fact(
        &self,
        query: &str,
        expected_fact_id: Uuid,
    ) -> Result<ironrag_backend::services::query::search::QueryEvidenceSearchResult> {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        let descriptive_ir = ironrag_backend::domains::query_ir::QueryIR {
            act: ironrag_backend::domains::query_ir::QueryAct::Describe,
            scope: ironrag_backend::domains::query_ir::QueryScope::SingleDocument,
            language: ironrag_backend::domains::query_ir::QueryLanguage::Auto,
            retrieval_query: None,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 1.0,
        };
        loop {
            let result = SearchService::new()
                .search_query_evidence(&self.state, self.library_id, query, &descriptive_ir, 5)
                .await
                .with_context(|| {
                    format!("failed to search query evidence for technical fact query {query}")
                })?;
            if result.technical_fact_hits.first().map(|row| row.fact_id) == Some(expected_fact_id) {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for top technical fact {} for query {}; last observed {:?}",
                    expected_fact_id,
                    query,
                    result.technical_fact_hits.first().map(|row| row.fact_id)
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
}

async fn mint_library_read_token(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<String> {
    let plaintext = format!("knowledge-search-{}", Uuid::now_v7());
    let token = iam_repository::create_api_token(
        postgres,
        Some(workspace_id),
        "knowledge-search",
        "knowledge-search",
        None,
        None,
    )
    .await
    .context("failed to create knowledge search api token")?;
    iam_repository::create_api_token_secret(postgres, token.principal_id, &hash_token(&plaintext))
        .await
        .context("failed to create knowledge search token secret")?;
    iam_repository::create_grant(
        postgres,
        token.principal_id,
        "library",
        library_id,
        PERMISSION_LIBRARY_READ,
        None,
        None,
    )
    .await
    .context("failed to create knowledge search grant")?;
    Ok(plaintext)
}

fn replace_database_name(base_database_url: &str, database_name: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_database_url)
        .with_context(|| format!("invalid postgres url: {base_database_url}"))?;
    let path = url.path().trim_matches('/');
    if path.is_empty() {
        return Err(anyhow!("postgres url must include a database name"));
    }
    url.set_path(database_name);
    Ok(url.to_string())
}

async fn terminate_database_connections(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(admin_pool)
    .await
    .context("failed to terminate postgres database connections")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn library_generation_signals_count_canonical_chunk_embedding_vectors() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let embedding_profile_key = synthetic_embedding_profile_key(model_catalog_id);
        let now = Utc::now();

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision".to_string()),
                source_uri: Some("memory://revision/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision".to_string(),
                title: Some("Generation Signal Fixture".to_string()),
                byte_size: 24,
                normalized_text: Some("generation signal fixture".to_string()),
                text_checksum: Some("generation-signal-fixture".to_string()),
                image_checksum: None,
                text_state: "accepted".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: None,
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert revision for generation signal fixture")?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: embedding_profile_key,
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.2, 0.4, 0.6],
                freshness_generation: 2,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert canonical chunk embedding vector")?;

        let signals = fixture
            .document_store
            .aggregate_library_generation_signals(library_id)
            .await
            .context("failed to aggregate library generation signals")?;
        assert_eq!(signals.active_vector_generation, 2);
        assert!(signals.has_ready_vector);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn canonical_chunk_reads_quarantine_legacy_raptor_rows() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let canonical_chunk_id = Uuid::now_v7();
        let legacy_raptor_chunk_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "raptor-quarantine-revision".to_string(),
                title: Some("Raptor quarantine fixture".to_string()),
                byte_size: 64,
                normalized_text: Some("canonical leaf evidence".to_string()),
                text_checksum: Some("raptor-quarantine-text".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await?;

        let base_chunk = KnowledgeChunkRow {
            chunk_id: canonical_chunk_id,
            workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_index: 0,
            content_text: "canonical leaf evidence".to_string(),
            normalized_text: "canonical leaf evidence".to_string(),
            span_start: Some(0),
            span_end: Some(23),
            token_count: Some(3),
            chunk_kind: Some("paragraph".to_string()),
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: None,
            quality_score: None,
            window_text: None,
            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        };
        fixture.document_store.upsert_chunk(&base_chunk).await?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: legacy_raptor_chunk_id,
                chunk_index: 42,
                chunk_kind: Some("raptor_summary".to_string()),
                content_text: "legacy synthetic raptor secretword".to_string(),
                normalized_text: "legacy synthetic raptor secretword".to_string(),
                raptor_level: Some(1),
                ..base_chunk.clone()
            })
            .await?;

        let by_revision = fixture.document_store.list_chunks_by_revision(revision_id).await?;
        assert_eq!(
            by_revision.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
            vec![canonical_chunk_id]
        );
        assert_eq!(fixture.document_store.count_chunks_by_revision(revision_id).await?, 1);
        assert!(fixture.document_store.get_chunk(legacy_raptor_chunk_id).await?.is_none());
        assert_eq!(
            fixture
                .document_store
                .list_chunks_by_ids(&[canonical_chunk_id, legacy_raptor_chunk_id])
                .await?
                .iter()
                .map(|chunk| chunk.chunk_id)
                .collect::<Vec<_>>(),
            vec![canonical_chunk_id]
        );
        assert!(
            fixture
                .document_store
                .list_chunks_by_revision_matching_terms(
                    revision_id,
                    &["secretword".to_string()],
                    10,
                )
                .await?
                .is_empty()
        );
        assert_eq!(
            fixture
                .document_store
                .list_tail_chunks_by_revision(revision_id, 10)
                .await?
                .iter()
                .map(|chunk| chunk.chunk_id)
                .collect::<Vec<_>>(),
            vec![canonical_chunk_id]
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn vector_ready_revisions_missing_chunk_vectors_are_counted() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let no_chunk_revision_id = Uuid::now_v7();
        let pending_revision_id = Uuid::now_v7();
        let pending_chunk_id = Uuid::now_v7();
        let superseded_revision_id = Uuid::now_v7();
        let superseded_chunk_id = Uuid::now_v7();
        let other_library_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();
        let other_chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let embedding_profile_key = synthetic_embedding_profile_key(model_catalog_id);
        let now = Utc::now();

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision".to_string()),
                source_uri: Some("memory://revision/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision".to_string(),
                title: Some("Vector Inventory Fixture".to_string()),
                byte_size: 24,
                normalized_text: Some("vector inventory fixture".to_string()),
                text_checksum: Some("vector-inventory-fixture".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert vector inventory revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "vector inventory fixture".to_string(),
                normalized_text: "vector inventory fixture".to_string(),
                span_start: Some(0),
                span_end: Some(24),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector inventory chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: no_chunk_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "no-chunk-revision".to_string(),
                title: Some("No Chunk Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("no chunk".to_string()),
                text_checksum: Some("no-chunk".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert no-chunk revision")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: pending_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "pending-revision".to_string(),
                title: Some("Pending Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("pending".to_string()),
                text_checksum: Some("pending".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert pending revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: pending_chunk_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_id: pending_revision_id,
                chunk_index: 0,
                content_text: "pending".to_string(),
                normalized_text: "pending".to_string(),
                span_start: Some(0),
                span_end: Some(7),
                token_count: Some(1),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert pending chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: superseded_revision_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "superseded".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "superseded-revision".to_string(),
                title: Some("Superseded Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("superseded".to_string()),
                text_checksum: Some("superseded".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: Some(revision_id),
                created_at: now,
            })
            .await
            .context("failed to insert superseded revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: superseded_chunk_id,
                workspace_id,
                library_id,
                document_id: Uuid::now_v7(),
                revision_id: superseded_revision_id,
                chunk_index: 0,
                content_text: "superseded".to_string(),
                normalized_text: "superseded".to_string(),
                span_start: Some(0),
                span_end: Some(10),
                token_count: Some(1),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert superseded chunk")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: other_revision_id,
                workspace_id,
                library_id: other_library_id,
                document_id: Uuid::now_v7(),
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "other-revision".to_string(),
                title: Some("Other Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("other".to_string()),
                text_checksum: Some("other".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert other-library revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: other_chunk_id,
                workspace_id,
                library_id: other_library_id,
                document_id: Uuid::now_v7(),
                revision_id: other_revision_id,
                chunk_index: 0,
                content_text: "other".to_string(),
                normalized_text: "other".to_string(),
                span_start: Some(0),
                span_end: Some(5),
                token_count: Some(1),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert other-library chunk")?;

        let stale_count = fixture
            .document_store
            .count_vector_ready_revisions_missing_chunk_vectors(library_id)
            .await
            .context("failed to count vector inventory mismatch")?;
        assert_eq!(stale_count, 1);

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: embedding_profile_key,
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.2, 0.4, 0.6],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector inventory row")?;
        let repaired_count = fixture
            .document_store
            .count_vector_ready_revisions_missing_chunk_vectors(library_id)
            .await
            .context("failed to count repaired vector inventory")?;
        assert_eq!(repaired_count, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn lexical_chunk_search_stays_library_scoped() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let target_library_id = Uuid::now_v7();
        let distractor_library_id = Uuid::now_v7();
        let target_document_id = Uuid::now_v7();
        let target_revision_id = Uuid::now_v7();
        let target_chunk_id = Uuid::now_v7();
        let distractor_document_id = Uuid::now_v7();
        let distractor_revision_id = Uuid::now_v7();
        let distractor_chunk_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id: target_document_id,
                workspace_id,
                library_id: target_library_id,
                external_key: "lexical-target".to_string(),
                file_name: None,
                title: Some("Target".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(target_revision_id),
                readable_revision_id: Some(target_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert target document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: target_revision_id,
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://target".to_string()),
                source_uri: Some("memory://target/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "target-checksum".to_string(),
                title: Some("Target".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("target-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert target revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: target_chunk_id,
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_id: target_revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert target chunk")?;

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id: distractor_document_id,
                workspace_id,
                library_id: distractor_library_id,
                external_key: "lexical-distractor".to_string(),
                file_name: None,
                title: Some("Distractor".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(distractor_revision_id),
                readable_revision_id: Some(distractor_revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert distractor document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: distractor_revision_id,
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://distractor".to_string()),
                source_uri: Some("memory://distractor/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "distractor-checksum".to_string(),
                title: Some("Distractor".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("distractor-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert distractor revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: distractor_chunk_id,
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_id: distractor_revision_id,
                chunk_index: 0,
                content_text: "orion lexical anchor".to_string(),
                normalized_text: "orion lexical anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert distractor chunk")?;

        let hits = fixture
            .wait_for_chunk_hits(target_library_id, "orion lexical anchor", &[target_chunk_id])
            .await?;
        assert_eq!(hits, vec![target_chunk_id]);
        let structured_chunks = fixture
            .document_store
            .list_chunks_by_revision(target_revision_id)
            .await
            .context("failed to reload structured chunks for ancestry assertion")?;
        let target_chunk = structured_chunks
            .into_iter()
            .find(|chunk| chunk.chunk_id == target_chunk_id)
            .ok_or_else(|| anyhow!("target chunk vanished before ancestry assertion"))?;
        assert_eq!(target_chunk.section_path, vec!["intro".to_string()]);
        assert_eq!(target_chunk.heading_trail, vec!["Intro".to_string()]);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn chunk_and_entity_vectors_roundtrip_with_generation_order() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let embedding_profile_key = synthetic_embedding_profile_key(model_catalog_id);
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "vector-doc".to_string(),
                file_name: None,
                title: Some("Vector Doc".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert vector test document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://vector-doc".to_string()),
                source_uri: Some("memory://vector-doc/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "vector-checksum".to_string(),
                title: Some("Vector Doc".to_string()),
                byte_size: 32,
                normalized_text: Some("vector generation anchor".to_string()),
                text_checksum: Some("vector-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert vector test revision")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "vector generation anchor".to_string(),
                normalized_text: "vector generation anchor".to_string(),
                span_start: Some(0),
                span_end: Some(20),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["intro".to_string()],
                heading_trail: vec!["Intro".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert vector test chunk")?;
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id,
                library_id,
                canonical_label: "VectorEntity".to_string(),
                aliases: vec!["Entity Alias".to_string()],
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: Some("Entity vector anchor".to_string()),
                confidence: Some(0.9),
                support_count: 2,
                freshness_generation: 2,
                entity_state: "active".to_string(),
                created_at: Some(now),
                updated_at: Some(now),
            })
            .await
            .context("failed to insert vector test entity")?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert generation 1 chunk vector")?;
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id,
                revision_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.7],
                freshness_generation: 2,
                created_at: now + chrono::TimeDelta::seconds(1),
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert generation 2 chunk vector")?;

        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![1.0, 1.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await
            .context("failed to insert generation 1 entity vector")?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: 3,
                vector: vec![2.0, 2.0, 2.0],
                freshness_generation: 2,
                created_at: now + chrono::TimeDelta::seconds(1),
            })
            .await
            .context("failed to insert generation 2 entity vector")?;

        let chunk_vectors = fixture
            .search_store
            .list_chunk_vectors_by_chunk(chunk_id)
            .await
            .context("failed to list chunk vectors")?;
        assert_eq!(chunk_vectors.len(), 2);
        assert_eq!(chunk_vectors[0].freshness_generation, 2);
        assert_eq!(chunk_vectors[1].freshness_generation, 1);
        assert_eq!(chunk_vectors[0].vector, vec![0.9, 0.8, 0.7]);

        let entity_vectors = fixture
            .search_store
            .list_entity_vectors_by_entity(entity_id)
            .await
            .context("failed to list entity vectors")?;
        assert_eq!(entity_vectors.len(), 2);
        assert_eq!(entity_vectors[0].freshness_generation, 2);
        assert_eq!(entity_vectors[1].freshness_generation, 1);
        assert_eq!(entity_vectors[0].vector, vec![2.0, 2.0, 2.0]);

        let current_entity_hits = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                3,
                library_id,
                &embedding_profile_key,
                &[2.0, 2.0, 2.0],
                8,
                None,
            )
            .await
            .context("failed to search current-generation entity vectors")?;
        assert_eq!(current_entity_hits.len(), 1);
        assert_eq!(current_entity_hits[0].entity_id, entity_id);
        assert_eq!(current_entity_hits[0].freshness_generation, 2);

        fixture
            .search_store
            .delete_entity_vector(entity_id, &embedding_profile_key, 2)
            .await
            .context("failed to remove current-generation entity vector")?
            .context("current-generation entity vector was missing")?;
        let stale_only_entity_hits = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                3,
                library_id,
                &embedding_profile_key,
                &[1.0, 1.0, 1.0],
                8,
                None,
            )
            .await
            .context("failed to search stale-only entity vector lane")?;
        assert!(
            stale_only_entity_hits.is_empty(),
            "an entity vector from an older freshness generation must never be query-visible"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn revision_replacement_updates_readiness_and_chunk_search_surface() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_one_id = Uuid::now_v7();
        let revision_two_id = Uuid::now_v7();
        let chunk_one_id = Uuid::now_v7();
        let chunk_two_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "replacement-doc".to_string(),
                file_name: None,
                title: Some("Replacement Doc".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_one_id),
                readable_revision_id: Some(revision_one_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert replacement document")?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: revision_one_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision-one".to_string()),
                source_uri: Some("memory://revision-one/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision-one".to_string(),
                title: Some("Revision One".to_string()),
                byte_size: 32,
                normalized_text: Some("obsolete nebula anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-1".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert revision one")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: chunk_one_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_one_id,
                chunk_index: 0,
                content_text: "obsolete nebula anchor".to_string(),
                normalized_text: "obsolete nebula anchor".to_string(),
                span_start: Some(0),
                span_end: Some(24),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["revision-one".to_string()],
                heading_trail: vec!["Revision One".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert revision one chunk")?;
        let old_hits = fixture
            .wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[chunk_one_id])
            .await?;
        assert_eq!(old_hits, vec![chunk_one_id]);

        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: revision_two_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "replace".to_string(),
                storage_ref: Some("memory://revision-two".to_string()),
                source_uri: Some("memory://revision-two/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "revision-two".to_string(),
                title: Some("Revision Two".to_string()),
                byte_size: 32,
                normalized_text: Some("fresh pulsar anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-2".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "ready".to_string(),
                text_readable_at: Some(now + chrono::TimeDelta::seconds(1)),
                vector_ready_at: Some(now + chrono::TimeDelta::seconds(1)),
                graph_ready_at: Some(now + chrono::TimeDelta::seconds(1)),
                superseded_by_revision_id: None,
                created_at: now + chrono::TimeDelta::seconds(1),
            })
            .await
            .context("failed to insert revision two")?;
        fixture
            .document_store
            .update_revision_readiness(
                revision_one_id,
                "superseded",
                "superseded",
                "superseded",
                Some(now),
                None,
                None,
                Some(revision_two_id),
            )
            .await
            .context("failed to supersede revision one readiness")?
            .ok_or_else(|| anyhow!("revision one disappeared during supersede update"))?;
        fixture
            .document_store
            .delete_chunks_by_revision(revision_one_id)
            .await
            .context("failed to delete revision one chunks")?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: chunk_two_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_two_id,
                chunk_index: 0,
                content_text: "fresh pulsar anchor".to_string(),
                normalized_text: "fresh pulsar anchor".to_string(),
                span_start: Some(0),
                span_end: Some(19),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["revision-two".to_string()],
                heading_trail: vec!["Revision Two".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(2),
                vector_generation: Some(2),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert revision two chunk")?;
        fixture
            .document_store
            .update_document_pointers(
                document_id,
                "active",
                Some(revision_two_id),
                Some(revision_two_id),
                Some(2),
                None,
                None,
            )
            .await
            .context("failed to update document pointers after replacement")?
            .ok_or_else(|| anyhow!("document disappeared during pointer update"))?;
        fixture.wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[]).await?;
        let new_hits =
            fixture.wait_for_chunk_hits(library_id, "fresh pulsar anchor", &[chunk_two_id]).await?;
        assert_eq!(new_hits, vec![chunk_two_id]);

        let document = fixture
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload document after replacement")?
            .ok_or_else(|| anyhow!("replacement document not found"))?;
        assert_eq!(document.active_revision_id, Some(revision_two_id));
        assert_eq!(document.readable_revision_id, Some(revision_two_id));
        assert_eq!(document.latest_revision_no, Some(2));

        let revision_one = fixture
            .document_store
            .get_revision(revision_one_id)
            .await
            .context("failed to reload revision one")?
            .ok_or_else(|| anyhow!("revision one not found"))?;
        assert_eq!(revision_one.superseded_by_revision_id, Some(revision_two_id));
        assert_eq!(revision_one.text_state, "superseded");

        let revision_two = fixture
            .document_store
            .get_revision(revision_two_id)
            .await
            .context("failed to reload revision two")?
            .ok_or_else(|| anyhow!("revision two not found"))?;
        assert_eq!(revision_two.vector_state, "vector_ready");
        assert_eq!(revision_two.graph_state, "graph_ready");
        assert!(revision_two.vector_ready_at.is_some());
        assert!(revision_two.graph_ready_at.is_some());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_documents_endpoint_returns_hybrid_knowledge_payload() -> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let body = fixture.search_document_hit("orion").await?;
        assert_eq!(body["libraryId"], json!(fixture.library_id));
        assert_eq!(body["queryText"], json!("orion"));
        assert_eq!(body["limit"], json!(5));
        assert_eq!(body["freshnessGeneration"], json!(1));
        assert_eq!(body["embeddingProviderKind"], json!("openai"));
        assert!(!body["embeddingModelName"].as_str().unwrap_or_default().is_empty());

        let document_hits =
            body["documentHits"].as_array().context("documentHits must be an array")?;
        assert_eq!(document_hits.len(), 1);
        let document_hit = &document_hits[0];
        assert_eq!(document_hit["document"]["documentId"], json!(fixture.document_id));
        assert_eq!(document_hit["revision"]["revisionId"], json!(fixture.revision_id));
        assert_eq!(document_hit["provenanceSummary"]["supportingEvidenceCount"], json!(1));
        assert_eq!(document_hit["provenanceSummary"]["lexicalChunkCount"], json!(1));
        assert_eq!(document_hit["provenanceSummary"]["vectorChunkCount"], json!(1));
        assert_eq!(document_hit["technicalFactSummary"]["typedFactCount"], json!(1));
        assert_eq!(
            document_hit["technicalFactSummary"]["factKindCounts"]["endpoint_path"],
            json!(1)
        );
        assert_eq!(document_hit["graphEvidenceSummary"]["evidenceCount"], json!(1));
        assert_eq!(document_hit["graphEvidenceSummary"]["factBackedCount"], json!(1));
        assert_eq!(document_hit["chunkHits"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["vectorChunkHits"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["evidenceSamples"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["technicalFactSamples"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(document_hit["technicalFactSamples"][0]["factId"], json!(fixture.fact_id));
        assert_eq!(document_hit["technicalFactSamples"][0]["displayValue"], json!("/orion/status"));

        let entity_hits = body["entityHits"].as_array().context("entityHits must be an array")?;
        assert_eq!(entity_hits.len(), 1);
        assert_eq!(entity_hits[0]["entityId"], json!(fixture.entity_id));
        assert_eq!(entity_hits[0]["canonicalLabel"], json!("Orion Signal"));

        let relation_hits =
            body["relationHits"].as_array().context("relationHits must be an array")?;
        assert_eq!(relation_hits.len(), 1);
        assert_eq!(relation_hits[0]["relationId"], json!(fixture.relation_id));
        assert_eq!(relation_hits[0]["canonicalLabel"], json!("Orion relation"));

        let vector_chunk_hits =
            body["vectorChunkHits"].as_array().context("vectorChunkHits must be an array")?;
        assert_eq!(vector_chunk_hits.len(), 1);
        assert_eq!(vector_chunk_hits[0]["chunkId"], json!(fixture.chunk_id));

        let vector_entity_hits =
            body["vectorEntityHits"].as_array().context("vectorEntityHits must be an array")?;
        assert_eq!(vector_entity_hits.len(), 1);
        assert_eq!(vector_entity_hits[0]["entityId"], json!(fixture.entity_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn lexical_chunk_search_ignores_non_chunk_documents_in_shared_search_view() -> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = fixture
                .state
                .search_store
                .search_chunks(fixture.library_id, "/orion/status", 8, None, None)
                .await
                .context("failed to run lexical chunk search against shared search view")?;
            let chunk_ids = hits.iter().map(|row| row.chunk_id).collect::<BTreeSet<_>>();
            if chunk_ids == BTreeSet::from([fixture.chunk_id]) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for lexical chunk search to return only the canonical chunk {}; last observed {:?}",
                    fixture.chunk_id,
                    chunk_ids
                ));
            }
            sleep(SEARCH_POLL_INTERVAL).await;
        }
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_query_evidence_ranks_typed_facts_for_url_endpoint_method_and_parameter_questions()
-> Result<()> {
    let fixture = KnowledgeSearchHttpFixture::create().await?;

    let result = async {
        let url_fact_id = Uuid::now_v7();
        let method_fact_id = Uuid::now_v7();
        let parameter_fact_id = Uuid::now_v7();
        let distractor_parameter_fact_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .state
            .document_store
            .replace_technical_facts(
                fixture.revision_id,
                &[
                    KnowledgeTechnicalFactRow {
                        fact_id: fixture.fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "endpoint_path".to_string(),
                        canonical_value_text: "/orion/status".to_string(),
                        canonical_value_exact: "/orion/status".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "/orion/status" }),
                        display_value: "/orion/status".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.98),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: url_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "url".to_string(),
                        canonical_value_text: "https://api.example.com/orion/status".to_string(),
                        canonical_value_exact: "https://api.example.com/orion/status".to_string(),
                        canonical_value_json: json!({
                            "value_type": "text",
                            "value": "https://api.example.com/orion/status"
                        }),
                        display_value: "https://api.example.com/orion/status".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.97),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: method_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "http_method".to_string(),
                        canonical_value_text: "GET".to_string(),
                        canonical_value_exact: "GET".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "GET" }),
                        display_value: "GET".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.96),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: parameter_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "parameter_name".to_string(),
                        canonical_value_text: "pageNumber".to_string(),
                        canonical_value_exact: "pageNumber".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
                        display_value: "pageNumber".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.95),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                    KnowledgeTechnicalFactRow {
                        fact_id: distractor_parameter_fact_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        document_id: fixture.document_id,
                        revision_id: fixture.revision_id,
                        fact_kind: "parameter_name".to_string(),
                        canonical_value_text: "pageSize".to_string(),
                        canonical_value_exact: "pageSize".to_string(),
                        canonical_value_json: json!({ "value_type": "text", "value": "pageSize" }),
                        display_value: "pageSize".to_string(),
                        qualifiers_json: json!([]),
                        support_block_ids: Vec::new(),
                        support_chunk_ids: vec![fixture.chunk_id],
                        confidence: Some(0.94),
                        extraction_kind: "fixture_seed".to_string(),
                        conflict_group_id: None,
                        created_at: now,
                        updated_at: now,
                    },
                ],
            )
            .await
            .context("failed to reseed canonical technical facts for ranking regression")?;

        let endpoint_result = fixture
            .wait_for_query_evidence_top_fact("/orion/status", fixture.fact_id)
            .await?;
        assert!(endpoint_result.exact_literal_bias);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_id, fixture.fact_id);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_kind, "endpoint_path");
        assert!(endpoint_result.technical_fact_hits[0].exact_match);

        let url_result = fixture
            .wait_for_query_evidence_top_fact(
                "https://api.example.com/orion/status",
                url_fact_id,
            )
            .await?;
        assert!(url_result.exact_literal_bias);
        assert_eq!(url_result.technical_fact_hits[0].fact_id, url_fact_id);
        assert_eq!(url_result.technical_fact_hits[0].fact_kind, "url");
        assert!(url_result.technical_fact_hits[0].exact_match);

        let method_result = fixture
            .wait_for_query_evidence_top_fact("HTTP method GET", method_fact_id)
            .await?;
        assert!(method_result.exact_literal_bias);
        assert_eq!(method_result.technical_fact_hits[0].fact_id, method_fact_id);
        assert_eq!(method_result.technical_fact_hits[0].fact_kind, "http_method");

        let parameter_result = fixture
            .wait_for_query_evidence_top_fact("query parameter pageNumber", parameter_fact_id)
            .await?;
        assert!(parameter_result.exact_literal_bias);
        assert_eq!(parameter_result.technical_fact_hits[0].fact_id, parameter_fact_id);
        assert_eq!(parameter_result.technical_fact_hits[0].fact_kind, "parameter_name");
        let distractor_position = parameter_result
            .technical_fact_hits
            .iter()
            .position(|row| row.fact_id == distractor_parameter_fact_id);
        if let Some(position) = distractor_position {
            assert!(position > 0);
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Two libraries on different embed dims must coexist without cross-library
/// or cross-dimension ANN leakage.
///
/// We go through the low-level `upsert_chunk_vector` API instead of
/// mocking the embed-binding pipeline end-to-end, so the test stays
/// small and focuses on the storage-layer isolation invariant.
#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn test_two_libraries_different_dims_isolated() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_a_id = Uuid::now_v7();
        let library_b_id = Uuid::now_v7();
        let chunk_a_id = Uuid::now_v7();
        let chunk_b_id = Uuid::now_v7();
        let revision_a_id = Uuid::now_v7();
        let revision_b_id = Uuid::now_v7();
        let profile_a_key = synthetic_embedding_profile_key(Uuid::now_v7());
        let profile_b_key = synthetic_embedding_profile_key(Uuid::now_v7());
        let now = Utc::now();

        // Library A writes a dim-3 vector.
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id: library_a_id,
                chunk_id: chunk_a_id,
                revision_id: revision_a_id,
                embedding_model_key: profile_a_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to upsert library A chunk vector")?;

        // Library B writes a dim-4 vector; it must not collide with library A's
        // dim-3 vector.
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id: library_b_id,
                chunk_id: chunk_b_id,
                revision_id: revision_b_id,
                embedding_model_key: profile_b_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 4,
                vector: vec![1.0, 0.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to upsert library B chunk vector")?;

        // ANN against dim-3 must return library A's chunk and nothing
        // else: library B's vector has a different library and dimension.
        let hits_a = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library_a_id,
                &profile_a_key,
                &[1.0, 0.0, 0.0],
                16,
                None,
                None,
                None,
            )
            .await
            .context("failed ANN search against library A dim-3 shard")?;
        assert_eq!(hits_a.len(), 1, "library A dim-3 shard must return exactly its own chunk");
        assert_eq!(hits_a[0].chunk_id, chunk_a_id);

        // ANN against dim-4 must return library B's chunk and nothing
        // else.
        let hits_b = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                4,
                library_b_id,
                &profile_b_key,
                &[1.0, 0.0, 0.0, 0.0],
                16,
                None,
                None,
                None,
            )
            .await
            .context("failed ANN search against library B dim-4 shard")?;
        assert_eq!(hits_b.len(), 1, "library B dim-4 shard must return exactly its own chunk");
        assert_eq!(hits_b[0].chunk_id, chunk_b_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn abandoned_vector_recovery_commits_progress_beyond_one_manifest_batch() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("vector-recovery-workspace-{suffix}"),
            "Vector Recovery Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("vector-recovery-library-{suffix}"),
            "Vector Recovery Library",
            None,
            None,
        )
        .await?;
        fixture.search_store.ensure_chunk_vector_shard(3).await?;

        let inserted = sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             )
             select $1, 3, $2, $3 || lpad(to_hex(sequence_no), 64, '0'), $4,
                    true, 0, false
             from generate_series(0::bigint, 512::bigint) sequence_no",
        )
        .bind(library.id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(VECTOR_REBUILD_STAGING_PROFILE_PREFIX)
        .bind("knowledge_chunk_vector_d3")
        .execute(&fixture.postgres)
        .await?;
        assert_eq!(inserted.rows_affected(), 513);

        assert_eq!(
            fixture.search_store.discard_abandoned_staged_vector_rebuilds(library.id).await?,
            513
        );
        let remaining = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1
               and promoted = false
               and embedding_model_key like $2",
        )
        .bind(library.id)
        .bind(format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}%"))
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with canonical extensions"]
async fn staged_vector_promotion_is_atomic_across_chunk_and_entity_lanes() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("staged-vector-workspace-{suffix}"),
            "Staged Vector Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("staged-vector-library-{suffix}"),
            "Staged Vector Library",
            None,
            None,
        )
        .await?;
        let canonical_profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let staging_seed = Uuid::now_v7();
        let staging_profile =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{0}{0}", staging_seed.simple());
        let abandoned_seed = Uuid::now_v7();
        let abandoned_profile =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{0}{0}", abandoned_seed.simple());
        let unprepared_seed = Uuid::now_v7();
        let unprepared_profile =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{0}{0}", unprepared_seed.simple());
        let old_chunk_id = Uuid::now_v7();
        let staged_chunk_id = old_chunk_id;
        let same_dimension_orphan_chunk_id = Uuid::now_v7();
        let old_entity_id = Uuid::now_v7();
        let staged_entity_id = old_entity_id;
        let same_dimension_orphan_entity_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: "staged-vector-document".to_string(),
                file_name: None,
                title: Some("Staged Vector Document".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await?;
        fixture.search_store.ensure_chunk_vector_shard(4).await?;
        sqlx::query(
            "insert into knowledge_chunk_vector_d4 (
                key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding,
                freshness_generation, created_at, occurred_at, occurred_until
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, 4, $9::vector(4), 1, $10, null, null)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(Uuid::now_v7())
        .bind(workspace.id)
        .bind(library.id)
        .bind(old_chunk_id)
        .bind(revision_id)
        .bind(&canonical_profile)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind("[1,0,0,0]")
        .bind(now)
        .execute(&fixture.postgres)
        .await?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://staged-vector-document".to_string()),
                source_uri: Some("memory://staged-vector-document/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: "staged-vector-checksum".to_string(),
                title: Some("Staged Vector Document".to_string()),
                byte_size: 32,
                normalized_text: Some("staged vector evidence".to_string()),
                text_checksum: Some("staged-vector-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: Some(now),
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await?;
        fixture
            .document_store
            .upsert_chunk(&KnowledgeChunkRow {
                chunk_id: old_chunk_id,
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: "staged vector evidence".to_string(),
                normalized_text: "staged vector evidence".to_string(),
                span_start: Some(0),
                span_end: Some(22),
                token_count: Some(3),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id: old_entity_id,
                workspace_id: workspace.id,
                library_id: library.id,
                canonical_label: "StagedEntity".to_string(),
                aliases: Vec::new(),
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: Some("staged vector evidence".to_string()),
                confidence: Some(0.9),
                support_count: 1,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(now),
                updated_at: Some(now),
            })
            .await?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: old_chunk_id,
                revision_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id: old_entity_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await?;
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: same_dimension_orphan_chunk_id,
                revision_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id: same_dimension_orphan_entity_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await?;

        let unprepared_write = fixture
            .search_store
            .upsert_chunk_vectors_bulk_deferred_manifest(&[KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: old_chunk_id,
                revision_id,
                embedding_model_key: unprepared_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 1.0, 0.0],
                freshness_generation: 2,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            }])
            .await;
        assert!(unprepared_write.is_err());
        let unprepared_vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&unprepared_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(unprepared_vector_count, 0);

        fixture
            .search_store
            .prepare_chunk_vector_rebuild_lane(library.id, 3, &abandoned_profile)
            .await?;
        fixture
            .search_store
            .prepare_entity_vector_rebuild_lane(library.id, 3, &abandoned_profile)
            .await?;
        let abandoned_rows = (0..512)
            .map(|_| KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: Uuid::now_v7(),
                revision_id,
                embedding_model_key: abandoned_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 2,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .collect::<Vec<_>>();
        fixture.search_store.upsert_chunk_vectors_bulk_deferred_manifest(&abandoned_rows).await?;
        fixture
            .search_store
            .reconcile_chunk_vector_manifest_count(library.id, 3, &abandoned_profile)
            .await?;
        let canonical_during_staging = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[1.0, 0.0, 0.0],
                1,
                Some(1),
                None,
                None,
            )
            .await?;
        assert_eq!(
            canonical_during_staging.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [old_chunk_id]
        );
        assert_eq!(
            fixture.search_store.discard_abandoned_staged_vector_rebuilds(library.id).await?,
            1
        );
        let abandoned_manifest_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&abandoned_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(abandoned_manifest_count, 0);
        let cleanup_chunk_orphan_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and chunk_id = $2",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_chunk_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(cleanup_chunk_orphan_count, 0);
        let cleanup_entity_orphan_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and entity_id = $2",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_entity_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(cleanup_entity_orphan_count, 0);
        let canonical_after_partial_cleanup = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[1.0, 0.0, 0.0],
                8,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(
            canonical_after_partial_cleanup.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [old_chunk_id]
        );

        // Recreate source-less rows so failed promotion proves rollback and a
        // successful promotion proves the same source-aware GC invariant.
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: same_dimension_orphan_chunk_id,
                revision_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id: same_dimension_orphan_entity_id,
                embedding_model_key: canonical_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await?;

        fixture
            .search_store
            .prepare_chunk_vector_rebuild_lane(library.id, 3, &staging_profile)
            .await?;
        fixture
            .search_store
            .prepare_entity_vector_rebuild_lane(library.id, 3, &staging_profile)
            .await?;
        fixture
            .search_store
            .upsert_chunk_vectors_bulk_deferred_manifest(&[KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: staged_chunk_id,
                revision_id,
                embedding_model_key: staging_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 1.0, 0.0],
                freshness_generation: 2,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            }])
            .await?;
        fixture
            .search_store
            .upsert_entity_vectors_bulk_deferred_manifest(&[KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                entity_id: staged_entity_id,
                embedding_model_key: staging_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.0, 1.0, 0.0],
                freshness_generation: 2,
                created_at: now,
            }])
            .await?;
        fixture
            .search_store
            .reconcile_chunk_vector_manifest_count(library.id, 3, &staging_profile)
            .await?;
        fixture
            .search_store
            .reconcile_entity_vector_manifest_count(library.id, 3, &staging_profile)
            .await?;

        let source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library.id).await?;
        let mut data_reader = fixture.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock_shared(hashtextextended($1::text, 0))")
            .bind(format!("{VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX}:{}", library.id))
            .execute(&mut *data_reader)
            .await?;
        let data_locked_promotion_store = fixture.search_store.clone();
        let data_locked_library_id = library.id;
        let data_locked_canonical_profile = canonical_profile.clone();
        let data_locked_staging_profile = staging_profile.clone();
        let mut data_locked_promotion = tokio::spawn(async move {
            data_locked_promotion_store
                .promote_staged_vector_rebuild(
                    data_locked_library_id,
                    3,
                    &data_locked_canonical_profile,
                    &data_locked_staging_profile,
                    source_version.saturating_add(1),
                    Some(1),
                    Some(1),
                )
                .await
        });
        assert!(
            timeout(Duration::from_millis(500), &mut data_locked_promotion).await.is_err(),
            "promotion must wait for the exclusive library vector-plane data lock"
        );
        data_reader.rollback().await?;
        let data_lock_result =
            data_locked_promotion.await.context("data-lock promotion task failed")?;
        assert!(data_lock_result.is_err());

        let mut config_serializer = fixture.postgres.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:ai-config-generation', 0)
             )",
        )
        .execute(&mut *config_serializer)
        .await?;
        let promotion_store = fixture.search_store.clone();
        let promotion_library_id = library.id;
        let promotion_canonical_profile = canonical_profile.clone();
        let promotion_staging_profile = staging_profile.clone();
        let mut serializer_blocked_promotion = tokio::spawn(async move {
            promotion_store
                .promote_staged_vector_rebuild(
                    promotion_library_id,
                    3,
                    &promotion_canonical_profile,
                    &promotion_staging_profile,
                    source_version.saturating_add(1),
                    Some(1),
                    Some(1),
                )
                .await
        });
        assert!(
            timeout(Duration::from_millis(500), &mut serializer_blocked_promotion).await.is_err(),
            "promotion must wait for the AI-config serializer"
        );
        let mut source_row_probe = fixture.postgres.begin().await?;
        let probed_library_id = sqlx::query_scalar::<_, Uuid>(
            "select id from catalog_library where id = $1 for update nowait",
        )
        .bind(library.id)
        .fetch_one(&mut *source_row_probe)
        .await
        .context("promotion locked the source row before the AI-config serializer")?;
        assert_eq!(probed_library_id, library.id);
        source_row_probe.rollback().await?;
        config_serializer.rollback().await?;
        let serializer_order_result =
            serializer_blocked_promotion.await.context("serializer-order promotion task failed")?;
        assert!(serializer_order_result.is_err());

        let stale_source_result = fixture
            .search_store
            .promote_staged_vector_rebuild(
                library.id,
                3,
                &canonical_profile,
                &staging_profile,
                source_version.saturating_add(1),
                Some(1),
                Some(1),
            )
            .await;
        assert!(stale_source_result.is_err());

        // Chunk promotion runs first inside the transaction. An entity count
        // failure therefore proves that the already-applied chunk mutations
        // roll back with the entity lane instead of exposing a split profile.
        let entity_count_failure = fixture
            .search_store
            .promote_staged_vector_rebuild(
                library.id,
                3,
                &canonical_profile,
                &staging_profile,
                source_version,
                Some(1),
                Some(2),
            )
            .await;
        assert!(entity_count_failure.is_err());

        let canonical_chunks = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[1.0, 0.0, 0.0],
                8,
                None,
                None,
                None,
            )
            .await?;
        let canonical_entities = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[1.0, 0.0, 0.0],
                8,
                None,
            )
            .await?;
        assert_eq!(
            canonical_chunks.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [old_chunk_id]
        );
        assert_eq!(
            canonical_entities.iter().map(|row| row.entity_id).collect::<Vec<_>>(),
            [old_entity_id]
        );

        let staged_manifest_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&staging_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(staged_manifest_count, 2);
        let orphan_count_after_rollback = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d4
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&canonical_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(orphan_count_after_rollback, 1);
        let same_dimension_chunk_orphan_after_rollback = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and chunk_id = $2 and embedding_model_key = $3",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_chunk_id)
        .bind(&canonical_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        let same_dimension_entity_orphan_after_rollback = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and entity_id = $2 and embedding_model_key = $3",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_entity_id)
        .bind(&canonical_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(same_dimension_chunk_orphan_after_rollback, 1);
        assert_eq!(same_dimension_entity_orphan_after_rollback, 1);

        fixture
            .search_store
            .promote_staged_vector_rebuild(
                library.id,
                3,
                &canonical_profile,
                &staging_profile,
                source_version,
                Some(1),
                Some(1),
            )
            .await?;

        let promoted_chunks = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[0.0, 1.0, 0.0],
                8,
                None,
                None,
                None,
            )
            .await?;
        let promoted_entities = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                3,
                library.id,
                &canonical_profile,
                &[0.0, 1.0, 0.0],
                8,
                None,
            )
            .await?;
        assert_eq!(
            promoted_chunks.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [staged_chunk_id]
        );
        assert_eq!(
            promoted_entities.iter().map(|row| row.entity_id).collect::<Vec<_>>(),
            [staged_entity_id]
        );
        let stored_chunks = fixture.search_store.list_chunk_vectors_by_chunk(old_chunk_id).await?;
        let stored_entities =
            fixture.search_store.list_entity_vectors_by_entity(old_entity_id).await?;
        assert_eq!(stored_chunks.len(), 1);
        assert_eq!(stored_chunks[0].freshness_generation, 2);
        assert_eq!(stored_chunks[0].vector, vec![0.0, 1.0, 0.0]);
        assert_eq!(stored_entities.len(), 1);
        assert_eq!(stored_entities[0].freshness_generation, 2);
        assert_eq!(stored_entities[0].vector, vec![0.0, 1.0, 0.0]);
        let orphan_count_after_promotion = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d4
             where library_id = $1",
        )
        .bind(library.id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(orphan_count_after_promotion, 0);
        let same_dimension_chunk_orphan_after_promotion = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and chunk_id = $2",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_chunk_id)
        .fetch_one(&fixture.postgres)
        .await?;
        let same_dimension_entity_orphan_after_promotion = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and entity_id = $2",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_entity_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(same_dimension_chunk_orphan_after_promotion, 0);
        assert_eq!(same_dimension_entity_orphan_after_promotion, 0);

        let manifest_profiles = sqlx::query_scalar::<_, String>(
            "select embedding_model_key
             from knowledge_vector_relation_manifest
             where library_id = $1
             order by vector_kind",
        )
        .bind(library.id)
        .fetch_all(&fixture.postgres)
        .await?;
        assert_eq!(manifest_profiles, vec![canonical_profile.clone(), canonical_profile]);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
