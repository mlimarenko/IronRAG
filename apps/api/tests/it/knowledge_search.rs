use crate::temp_postgres_database;

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Instant, sleep, timeout};
use tower::ServiceExt;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    domains::ai::AiBindingPurpose,
    infra::repositories::{self, ai_repository, iam_repository},
    infra::{
        knowledge_plane::{
            ChunkLexicalQuery, DocumentPointersUpdate, DocumentStore, EvidenceEdgeTargets,
            GraphStore, IngestAttemptMutationOutcome, IngestVectorWriteFence, LexicalQueryOrigin,
            RevisionReadinessUpdate, SearchStore, StagedVectorRebuildPromotion,
            VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX, VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
            VectorSimilaritySearchInput, VectorWriteFence,
        },
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkRow,
            KnowledgeChunkVectorRow, KnowledgeDocumentRow, KnowledgeEntityVectorRow,
            KnowledgeRevisionRow, KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow,
            KnowledgeTechnicalFactRow, NewKnowledgeEntity, NewKnowledgeEvidence,
        },
        postgres::{
            pg_document_store::PgDocumentStore, pg_graph_store::PgGraphStore,
            pg_search_store::PgSearchStore,
        },
    },
    integrations::llm::{EmbeddingRequest, EmbeddingResponse, LlmGateway},
    interfaces::http::{auth::hash_token, authorization::PERMISSION_LIBRARY_READ, router},
    services::query::search::SearchService,
    shared::{auth_tokens::mint_plaintext_api_token, secret_encryption::SecretPurpose},
};

const SEARCH_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

fn synthetic_embedding_profile_key(seed: Uuid) -> String {
    format!("embedding-profile:v1:{0}{0}", seed.simple())
}

fn synthetic_staging_profile_key(source_truth_version: i64, seed: Uuid) -> String {
    let digest = format!("{0}{0}", seed.simple());
    format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{source_truth_version:016x}{}", &digest[..48])
}

struct PreparedStructuredRevisionSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    source_text: &'a str,
    chunk_count: i32,
}

async fn seed_prepared_structured_revision(
    store: &PgDocumentStore,
    seed: PreparedStructuredRevisionSeed<'_>,
) -> Result<()> {
    let PreparedStructuredRevisionSeed {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        source_text,
        chunk_count,
    } = seed;
    let now = Utc::now();
    let span_end = i32::try_from(source_text.chars().count())
        .context("prepared search fixture text length exceeds i32")?;
    store
        .replace_structured_revision_artifact(
            &KnowledgeStructuredRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                preparation_state: "prepared".to_string(),
                normalization_profile: "knowledge-search-test-v1".to_string(),
                source_format: "text".to_string(),
                language_code: None,
                block_count: 1,
                chunk_count,
                typed_fact_count: 0,
                outline_json: json!([]),
                prepared_at: now,
                updated_at: now,
            },
            &[KnowledgeStructuredBlockRow {
                block_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 0,
                block_kind: "paragraph".to_string(),
                text: source_text.to_string(),
                normalized_text: source_text.to_string(),
                heading_trail: Vec::new(),
                section_path: Vec::new(),
                page_number: None,
                span_start: Some(0),
                span_end: Some(span_end),
                parent_block_id: None,
                table_coordinates_json: None,
                code_language: None,
                created_at: now,
                updated_at: now,
            }],
        )
        .await?;
    publish_declared_search_sources(&store.pool, library_id).await?;
    Ok(())
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

/// Creates another library inside an existing workspace.
///
/// The knowledge plane is keyed by `(library_id, workspace_id)` against
/// `catalog_library`, so a second library used alongside a first one has to
/// share its workspace — inventing either half builds a state the product
/// cannot produce.
async fn create_vector_test_library_in_workspace(
    fixture: &KnowledgeSearchFixture,
    workspace_id: Uuid,
    label: &str,
) -> Result<Uuid> {
    let suffix = Uuid::now_v7().simple().to_string();
    let library = repositories::catalog_repository::create_library(
        &fixture.postgres,
        workspace_id,
        &format!("{label}-library-{suffix}"),
        "Vector Fence Library",
        None,
        None,
    )
    .await?;
    Ok(library.id)
}

struct VectorInventoryChunkSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    content: &'a str,
}

/// Seeds one ready vector-inventory chunk. The vector-gap filter only cares that
/// a revision holds chunks; deriving the span from the content keeps each fixture
/// row self-consistent instead of repeating a field-by-field literal per case.
async fn seed_vector_inventory_chunk(
    fixture: &KnowledgeSearchFixture,
    seed: VectorInventoryChunkSeed<'_>,
) -> Result<()> {
    let VectorInventoryChunkSeed {
        workspace_id,
        library_id,
        document_id,
        revision_id,
        chunk_id,
        content,
    } = seed;
    fixture
        .document_store
        .upsert_chunk(&KnowledgeChunkRow {
            chunk_id,
            workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_index: 0,
            content_text: content.to_string(),
            normalized_text: content.to_string(),
            span_start: Some(0),
            span_end: Some(i32::try_from(content.chars().count()).unwrap_or(i32::MAX)),
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
            occurred_at: None,
            occurred_until: None,
        })
        .await
        .context("failed to seed vector inventory chunk")?;
    Ok(())
}

type HnswSettingsTuple = (String, String, String, String);

async fn fetch_hnsw_settings(pool: &PgPool) -> Result<HnswSettingsTuple> {
    let hnsw_settings =
        "select current_setting('hnsw.ef_search'), current_setting('hnsw.iterative_scan'),
        current_setting('hnsw.max_scan_tuples'), current_setting('hnsw.scan_mem_multiplier')";
    let row: HnswSettingsTuple = sqlx::query_as(hnsw_settings).fetch_one(pool).await?;
    Ok(row)
}

async fn fetch_statement_counters(pool: &PgPool) -> Result<(i64, i64)> {
    let counts: (i64, i64) = sqlx::query_as(
        "select coalesce(sum(generic_plans), 0)::bigint,
                coalesce(sum(custom_plans), 0)::bigint
         from pg_prepared_statements
         where position('knowledge_chunk_vector_d3' in statement) > 0
           and position('limit $7' in statement) > 0",
    )
    .fetch_one(pool)
    .await?;
    Ok(counts)
}

struct VectorPlanVerificationTarget<'a> {
    library_id: Uuid,
    embedding_model_key: &'a str,
    query_vector: &'a [f32],
    expected_chunk_id: Uuid,
}

async fn verify_primary_chunk_search_plan(
    search_store: &PgSearchStore,
    pool: &PgPool,
    target: &VectorPlanVerificationTarget<'_>,
    hnsw_before: &HnswSettingsTuple,
) -> Result<(i64, i64)> {
    let hits = search_store
        .search_chunk_vectors_by_similarity(
            VectorSimilaritySearchInput {
                dim: 3,
                library_id: target.library_id,
                embedding_model_key: target.embedding_model_key,
                query_vector: target.query_vector,
                n_probe: None,
            },
            16,
            None,
            None,
        )
        .await
        .context("failed primary chunk search under planted generic session policy")?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, target.expected_chunk_id);

    let restored_mode: String =
        sqlx::query_scalar("select current_setting('plan_cache_mode')").fetch_one(pool).await?;
    assert_eq!(restored_mode, "force_generic_plan");

    let (generic_plans, custom_plans) = fetch_statement_counters(pool).await?;
    assert_eq!(generic_plans, 1, "only SQLx's initial describe may use the planted generic policy");
    assert!(custom_plans >= 1, "the vector statement must execute as a custom plan");

    let hnsw_after = fetch_hnsw_settings(pool).await?;
    assert_eq!(&hnsw_after, hnsw_before);

    Ok((generic_plans, custom_plans))
}

async fn verify_document_diversity_search_plan(
    search_store: &PgSearchStore,
    pool: &PgPool,
    target: &VectorPlanVerificationTarget<'_>,
    hnsw_before: &HnswSettingsTuple,
    prev_counts: (i64, i64),
) -> Result<()> {
    let documents = search_store
        .search_chunk_vectors_by_document_similarity(
            VectorSimilaritySearchInput {
                dim: 3,
                library_id: target.library_id,
                embedding_model_key: target.embedding_model_key,
                query_vector: target.query_vector,
                n_probe: None,
            },
            1,
            None,
            None,
        )
        .await?;
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].chunk_id, target.expected_chunk_id);

    let (prev_generic, prev_custom) = prev_counts;
    let document_counts = fetch_statement_counters(pool).await?;
    assert!(
        document_counts.0 > prev_generic,
        "document-diversity must execute under the generic session policy"
    );
    assert_eq!(
        document_counts.1, prev_custom,
        "document-diversity must not add a custom-plan execution"
    );

    let mode: String =
        sqlx::query_scalar("select current_setting('plan_cache_mode')").fetch_one(pool).await?;
    assert_eq!(mode, "force_generic_plan");

    let document_settings = fetch_hnsw_settings(pool).await?;
    assert_eq!(&document_settings, hnsw_before);

    Ok(())
}

async fn assert_primary_chunk_search_overrides_generic_plan_transaction_locally(
    database_url: &str,
    library_id: Uuid,
    embedding_model_key: &str,
    query_vector: &[f32],
    expected_chunk_id: Uuid,
) -> Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .context("failed to open single-connection vector plan test pool")?;
    sqlx::query("set plan_cache_mode = force_generic_plan")
        .execute(&pool)
        .await
        .context("failed to plant a generic session plan policy")?;

    let search_store = PgSearchStore { pool: pool.clone() };
    sqlx::query("select '[1,0,0]'::vector").execute(&pool).await?;
    let hnsw_before = fetch_hnsw_settings(&pool).await?;
    let target = VectorPlanVerificationTarget {
        library_id,
        embedding_model_key,
        query_vector,
        expected_chunk_id,
    };

    let prev_counts =
        verify_primary_chunk_search_plan(&search_store, &pool, &target, &hnsw_before).await?;

    verify_document_diversity_search_plan(&search_store, &pool, &target, &hnsw_before, prev_counts)
        .await?;

    sqlx::query("reset plan_cache_mode").execute(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
async fn exact_profile_dimension_claim_is_durable_unique_and_ignores_legacy_keys() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        // A vector manifest row is owned by a real library: the schema now
        // enforces that, and a fixture that invents an id would be testing a
        // state the product cannot create.
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "manifest-claim").await?;
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

        let rebuild_profile = synthetic_staging_profile_key(1, Uuid::now_v7());
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
        // The claim must ignore a manifest that belongs to a different library —
        // a real one, since the manifest is owned by `catalog_library`.
        let unrelated_library_id =
            create_vector_test_library_in_workspace(&fixture, workspace_id, "unrelated-manifest")
                .await?;
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, 3, $2, $3, 'knowledge_chunk_vector_d3', false, 0, false)",
        )
        .bind(unrelated_library_id)
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
#[ignore = "requires local postgres service with required extensions"]
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
#[ignore = "requires local postgres service with required extensions"]
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
                &VectorWriteFence {
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
#[ignore = "requires local postgres service with required extensions"]
async fn revision_scoped_vector_delete_survives_unrelated_source_advance() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "vector-delete-race").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let revision_id = Uuid::now_v7();
        let rows = (0..3)
            .map(|index| KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id: Uuid::now_v7(),
                revision_id,
                embedding_model_key: profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1 + index as f32 * 0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .collect::<Vec<_>>();
        fixture.search_store.upsert_chunk_vectors_bulk(&rows).await?;

        // The caller captures the library generation, then a live connector
        // writing another document of the same library advances it before the
        // destructive delete takes its exclusive lock.
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
        let live_source_version =
            repositories::catalog_repository::get_library_source_truth_version(
                &fixture.postgres,
                library_id,
            )
            .await?;
        assert!(live_source_version > stale_source_version);

        let outcome = fixture
            .search_store
            .delete_chunk_vectors_by_revision_fenced(library_id, revision_id, stale_source_version)
            .await
            .context("an unrelated library source advance must not refuse the delete")?;
        assert_eq!(outcome.deleted, rows.len() as u64);
        assert!(outcome.source_truth_version > live_source_version);

        let remaining = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and revision_id = $2",
        )
        .bind(library_id)
        .bind(revision_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(remaining, 0, "every vector of the deleted revision must be gone");
        let manifest_count = sqlx::query_scalar::<_, i64>(
            "select coalesce(sum(row_count), 0)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library_id)
        .bind(&profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(manifest_count, 0, "the deleted revision manifest lane must reconcile to zero");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
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
                    &VectorWriteFence {
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
#[ignore = "requires local postgres service with required extensions"]
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
                    cleanup.source_truth_version.saturating_add(1),
                )
                .await
                .is_err(),
            "a source fence ahead of the observed generation must not authorize cleanup",
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
#[ignore = "requires local postgres service with required extensions"]
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
                &VectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(IngestVectorWriteFence {
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
                &VectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(IngestVectorWriteFence {
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
                IngestVectorWriteFence { attempt_id: old_attempt_id, revision_id },
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
                &VectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile.clone(),
                    ingest_attempt: Some(IngestVectorWriteFence {
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

        let stale_revision_cleanup = fixture
            .search_store
            .delete_attempt_owned_chunk_vectors_by_revision_fenced(
                library_id,
                source_version,
                IngestVectorWriteFence { attempt_id: old_attempt_id, revision_id },
            )
            .await?;
        assert_eq!(
            stale_revision_cleanup,
            IngestAttemptMutationOutcome::AuthorityLost,
            "an older attempt must not delete vectors adopted by the latest attempt",
        );
        let vector_count_after_stale_revision_cleanup = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and vector_id = $2",
        )
        .bind(library_id)
        .bind(new_vector_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(vector_count_after_stale_revision_cleanup, 1);
        let manifest_after_stale_revision_cleanup = sqlx::query_scalar::<_, i64>(
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
        assert_eq!(manifest_after_stale_revision_cleanup, 1);

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
                &VectorWriteFence {
                    expected_source_truth_version: source_version,
                    embedding_profile_key: profile,
                    ingest_attempt: Some(IngestVectorWriteFence {
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
                IngestVectorWriteFence { attempt_id: new_attempt_id, revision_id },
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
#[ignore = "requires local postgres service with required extensions"]
async fn graph_projection_reset_is_atomic_and_reconciles_entity_vector_manifest() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "entity-vector-delete").await?;
        let profile = synthetic_embedding_profile_key(Uuid::now_v7());
        let entity_ids = [Uuid::now_v7(), Uuid::now_v7()];
        for (entity_id, vector) in
            entity_ids.into_iter().zip([vec![0.1, 0.2, 0.3], vec![0.3, 0.2, 0.1]])
        {
            fixture
                .graph_store
                .upsert_entity(&NewKnowledgeEntity {
                    entity_id,
                    workspace_id,
                    library_id,
                    primary_label: format!("Graph reset entity {entity_id}"),
                    aliases: Vec::new(),
                    entity_type: "concept".to_string(),
                    entity_sub_type: None,
                    summary: None,
                    confidence: Some(1.0),
                    support_count: 1,
                    freshness_generation: 1,
                    entity_state: "active".to_string(),
                    created_at: Some(Utc::now()),
                    updated_at: Some(Utc::now()),
                })
                .await?;
            fixture
                .search_store
                .upsert_entity_vector(&KnowledgeEntityVectorRow {
                    vector_id: Uuid::now_v7(),
                    workspace_id,
                    library_id,
                    entity_id,
                    embedding_model_key: profile.clone(),
                    vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                    dimensions: 3,
                    vector,
                    freshness_generation: 1,
                    created_at: Utc::now(),
                })
                .await?;
        }
        // Replace the canonical cascade with a non-canonical same-name FK.
        // The prepare lane must drop it and re-attach a validated `on delete
        // cascade`; orphaned rows are no longer removed implicitly, so this
        // lane exercises the replacement against an exact active manifest.
        sqlx::query(
            "alter table knowledge_entity_vector_d3
             drop constraint knowledge_entity_vector_d3_library_id_workspace_id_fkey",
        )
        .execute(&fixture.postgres)
        .await?;
        sqlx::query(
            "alter table knowledge_entity_vector_d3
             add constraint knowledge_entity_vector_d3_library_id_workspace_id_fkey
             foreign key (library_id, workspace_id)
             references catalog_library (id, workspace_id)
             on delete restrict
             not valid",
        )
        .execute(&fixture.postgres)
        .await?;
        let source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library_id).await?;
        let staging_profile = format!(
            "embedding-rebuild:v1:{source_version:016x}{}",
            "b".repeat(48)
        );
        fixture
            .search_store
            .prepare_entity_vector_rebuild_lane(library_id, 3, &staging_profile)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select row_count
                 from knowledge_vector_relation_manifest
                 where library_id = $1
                   and embedding_model_key = $2
                   and vector_kind = $3",
            )
            .bind(library_id)
            .bind(&profile)
            .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
            .fetch_one(&fixture.postgres)
            .await?,
            2,
            "FK installation must preserve the exact active manifest count",
        );
        let canonical_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_entity_vector_d3_library_id_workspace_id_fkey'
                   and conrelid = 'knowledge_entity_vector_d3'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(&fixture.postgres)
        .await?;
        assert!(canonical_cascade, "the non-canonical same-name FK must be replaced");
        sqlx::raw_sql(
            "create function fail_graph_reset_after_prior_deletes() returns trigger
             language plpgsql as $$
             begin
                 raise exception 'planted graph reset rollback';
             end
             $$;
             create trigger fail_graph_reset_after_prior_deletes
             before delete on runtime_graph_canonical_summary
             for each statement execute function fail_graph_reset_after_prior_deletes();",
        )
        .execute(&fixture.postgres)
        .await?;
        let rejected_reset_error = repositories::knowledge_graph_projection_repository::reset_library_materialized_graph_and_vectors(
            &fixture.postgres,
            library_id,
            source_version,
        )
        .await
        .expect_err("the graph reset rollback plant must fire");
        assert!(
            format!("{rejected_reset_error:#}").contains("planted graph reset rollback"),
            "the late trigger must be the rejecting failure: {rejected_reset_error:#}"
        );
        let rollback_state = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "select
                 (select count(*)::bigint from knowledge_entity_vector_d3 where library_id = $1),
                 (select row_count from knowledge_vector_relation_manifest
                  where library_id = $1 and embedding_model_key = $2 and vector_kind = $3),
                 (select count(*)::bigint from knowledge_entity where library_id = $1),
                 (select source_truth_version from catalog_library where id = $1)",
        )
        .bind(library_id)
        .bind(&profile)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(
            rollback_state,
            (2, 2, 2, source_version),
            "a late graph-table failure must roll back vectors, manifest, entities, and epoch",
        );
        sqlx::raw_sql(
            "drop trigger fail_graph_reset_after_prior_deletes on runtime_graph_canonical_summary;
             drop function fail_graph_reset_after_prior_deletes();",
        )
        .execute(&fixture.postgres)
        .await?;
        fixture
            .search_store
            .upsert_entity_vectors_bulk_deferred_manifest(&[KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id: entity_ids[0],
                embedding_model_key: staging_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.2, 0.3, 0.1],
                freshness_generation: 2,
                created_at: Utc::now(),
            }])
            .await?;
        let mut shared_vector_writer = fixture.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock_shared(hashtextextended($1::text, 0))")
            .bind(format!("{VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX}:{library_id}"))
            .execute(&mut *shared_vector_writer)
            .await?;
        let reset_postgres = fixture.postgres.clone();
        let mut reset_task = tokio::spawn(async move {
            repositories::knowledge_graph_projection_repository::reset_library_materialized_graph_and_vectors(
                &reset_postgres,
                library_id,
                source_version,
            )
            .await
        });
        assert!(
            timeout(Duration::from_millis(200), &mut reset_task).await.is_err(),
            "graph reset must wait behind a deferred writer's shared vector-plane lock"
        );
        shared_vector_writer.commit().await?;
        let outcome = timeout(Duration::from_secs(5), reset_task)
            .await
            .context("graph reset did not finish after the shared writer lock was released")?
            .context("graph reset task failed to join")??;
        assert_eq!(outcome.deleted, 3);
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
        let entity_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_entity where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(entity_count, 0);

        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id: entity_ids[0],
                workspace_id,
                library_id,
                primary_label: "Graph reset entity recreated with the same id".to_string(),
                aliases: Vec::new(),
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: None,
                confidence: Some(1.0),
                support_count: 1,
                freshness_generation: 2,
                entity_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await?;

        let stale_write = fixture
            .search_store
            .upsert_entity_vectors_bulk_deferred_manifest(&[KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                entity_id: entity_ids[0],
                embedding_model_key: staging_profile,
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.2, 0.3],
                freshness_generation: 1,
                created_at: Utc::now(),
            }])
            .await;
        assert!(
            stale_write.is_err(),
            "a deferred writer must reject the old source epoch even after the entity id reappears",
        );
        let stale_vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and entity_id = $2",
        )
        .bind(library_id)
        .bind(entity_ids[0])
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(stale_vector_count, 0, "the rejected stale batch must leave no physical row");

        let dormant_library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace_id,
            &format!("dormant-vector-library-{}", Uuid::now_v7().simple()),
            "Dormant Vector Library",
            None,
            None,
        )
        .await?;
        let dormant_entity_id = Uuid::now_v7();
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id: dormant_entity_id,
                workspace_id,
                library_id: dormant_library.id,
                primary_label: "Dormant shard entity".to_string(),
                aliases: Vec::new(),
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: None,
                confidence: Some(1.0),
                support_count: 1,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await?;
        fixture
            .search_store
            .upsert_entity_vector(&KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id: dormant_library.id,
                entity_id: dormant_entity_id,
                embedding_model_key: profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.3, 0.2, 0.1],
                freshness_generation: 1,
                created_at: Utc::now(),
            })
            .await?;
        sqlx::raw_sql(
            "alter table knowledge_entity_vector_d3
                 drop constraint knowledge_entity_vector_d3_library_id_workspace_id_fkey;
             alter table knowledge_entity_vector_d3
                 add constraint dormant_vector_restrict_fkey
                 foreign key (library_id, workspace_id)
                 references catalog_library (id, workspace_id)
                 on delete restrict;",
        )
        .execute(&fixture.postgres)
        .await?;
        let dormant_delete = repositories::catalog_repository::delete_library(
            &fixture.postgres,
            dormant_library.id,
        )
        .await?;
        assert_eq!(
            dormant_delete,
            repositories::catalog_repository::CatalogLibraryDeleteOutcome::Deleted,
            "catalog deletion must repair a dormant shard without an ensure call"
        );
        let dormant_vector_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1",
        )
        .bind(dormant_library.id)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(dormant_vector_count, 0, "catalog cascade must remove dormant shard rows");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

fn test_credential_master_key() -> String {
    STANDARD.encode([31_u8; 32])
}

struct VectorPairSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: &'a str,
    vector: Vec<f32>,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
}

async fn upsert_vector_pair(search_store: &PgSearchStore, seed: VectorPairSeed<'_>) -> Result<()> {
    search_store
        .upsert_chunk_vector(&KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: seed.workspace_id,
            library_id: seed.library_id,
            chunk_id: seed.chunk_id,
            revision_id: seed.revision_id,
            embedding_model_key: seed.embedding_model_key.to_string(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: seed.vector.clone(),
            freshness_generation: seed.freshness_generation,
            created_at: seed.created_at,
            occurred_at: None,
            occurred_until: None,
        })
        .await?;
    search_store
        .upsert_entity_vector(&KnowledgeEntityVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: seed.workspace_id,
            library_id: seed.library_id,
            entity_id: seed.entity_id,
            embedding_model_key: seed.embedding_model_key.to_string(),
            vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
            dimensions: 3,
            vector: seed.vector,
            freshness_generation: seed.freshness_generation,
            created_at: seed.created_at,
        })
        .await?;
    Ok(())
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
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "knowledge_search_http").await?;
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
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed knowledge search AI catalog")?;

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
        publish_declared_search_sources(&self.postgres, library_id).await?;
        let expected = expected_chunk_ids.iter().copied().collect::<BTreeSet<_>>();
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = self
                .search_store
                .search_chunks(ChunkLexicalQuery {
                    library_id,
                    query,
                    query_origin: LexicalQueryOrigin::UserQuery,
                    limit: expected_chunk_ids.len().max(8),
                    temporal_start: None,
                    temporal_end: None,
                    text_search_config: None,
                })
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

/// Seeds one document across both planes the metadata lane depends on.
///
/// The content plane always stays `active`: the divergence these tests are
/// about is a content row that outlived its knowledge row, so the knowledge
/// state is the only thing callers vary.
async fn seed_metadata_search_document(
    postgres: &sqlx::PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    file_name: &str,
    revision_title: &str,
    knowledge_state: &str,
) -> Result<Uuid> {
    let external_key = Uuid::now_v7().to_string();
    let document = repositories::content_document_repository::create_document(
        postgres,
        &repositories::content_document_repository::NewContentDocument {
            workspace_id,
            library_id,
            external_key: &external_key,
            document_state: "active",
            created_by_principal_id: None,
            parent_external_key: None,
            parent_document_id: None,
            document_role: "primary",
        },
    )
    .await?;
    sqlx::query("update content_document set source_file_name = $2 where id = $1")
        .bind(document.id)
        .bind(file_name)
        .execute(postgres)
        .await?;
    let revision = crate::content_repository::create_imported_fixture_revision(
        postgres,
        &repositories::content_revision_repository::NewContentRevision {
            admission_identity: None,
            document_id: document.id,
            workspace_id,
            library_id,
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "upload",
            checksum: &format!("sha256:{}", "9".repeat(64)),
            mime_type: "application/pdf",
            byte_size: 1,
            title: Some(revision_title),
            language_code: None,
            source_uri: None,
            document_hint: None,
            storage_key: None,
            created_by_principal_id: None,
        },
        file_name,
    )
    .await?;
    repositories::content_document_head_repository::upsert_document_head(
        postgres,
        &repositories::content_document_head_repository::NewContentDocumentHead {
            publishing_mutation_item_id: None,
            document_id: document.id,
            active_revision_id: Some(revision.id),
            readable_revision_id: Some(revision.id),
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    sqlx::query(
        "insert into knowledge_document (
            document_id, workspace_id, library_id, external_key, file_name, title,
            document_state, active_revision_id, readable_revision_id, latest_revision_no,
            parent_document_id, document_role, created_at, updated_at, deleted_at
         ) values ($1, $2, $3, $4, $5, null, $6, null, null, null,
                   null, 'primary', now(), now(),
                   case when $6 = 'deleted' then now() else null end)",
    )
    .bind(document.id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(&external_key)
    .bind(file_name)
    .bind(knowledge_state)
    .execute(postgres)
    .await?;
    // The canonical current view also requires a text-readable knowledge
    // revision. Keep the document-state divergence this fixture tests intact.
    sqlx::query("insert into knowledge_revision (revision_id,workspace_id,library_id,document_id,
        revision_number,revision_state,revision_kind,mime_type,checksum,title,byte_size,
        text_state,text_readable_at,vector_state,graph_state,created_at)
        select id,workspace_id,library_id,document_id,revision_number,'active',content_source_kind::text,
        mime_type,checksum,title,byte_size,'text_readable',now(),'accepted','accepted',created_at
        from content_revision where id=$1")
        .bind(revision.id).execute(postgres).await?;
    Ok(document.id)
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
async fn metadata_lane_skips_a_document_the_knowledge_plane_calls_deleted() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("metadata-plane-workspace-{suffix}"),
            "Metadata Plane Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("metadata-plane-library-{suffix}"),
            "Metadata Plane Library",
            None,
            None,
        )
        .await?;

        for (file_name, knowledge_state) in
            [("plane-beacon-live.pdf", "active"), ("plane-beacon-retired.pdf", "deleted")]
        {
            seed_metadata_search_document(
                &fixture.postgres,
                workspace.id,
                library.id,
                file_name,
                "Plane Manual",
                knowledge_state,
            )
            .await?;
        }

        let rows = repositories::content_metadata_search_repository::search_document_metadata_rows(
            &fixture.postgres,
            library.id,
            "plane-beacon",
            10,
        )
        .await?;

        // Asserting the live document survives is what stops an empty result
        // from passing this test for the wrong reason.
        assert_eq!(
            rows.iter().map(|row| row.matched_text.as_str()).collect::<Vec<_>>(),
            vec!["plane-beacon-live.pdf"],
            "the lane must drop the document the knowledge plane retired and keep the other: \
             readability is derived from that plane, so a row it calls deleted has no readiness \
             for the search to describe"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
async fn metadata_filename_search_is_literal_and_orders_versions_from_the_matching_field()
-> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &fixture.postgres,
            &format!("metadata-file-workspace-{suffix}"),
            "Metadata File Workspace",
            None,
        )
        .await?;
        let library = repositories::catalog_repository::create_library(
            &fixture.postgres,
            workspace.id,
            &format!("metadata-file-library-{suffix}"),
            "Metadata File Library",
            None,
            None,
        )
        .await?;

        for file_name in ["guide-2.0.pdf", "guide-10.0.pdf"] {
            seed_metadata_search_document(
                &fixture.postgres,
                workspace.id,
                library.id,
                file_name,
                "Shared Manual",
                "active",
            )
            .await?;
        }

        let rows = repositories::content_metadata_search_repository::search_document_metadata_rows(
            &fixture.postgres,
            library.id,
            "guide",
            10,
        )
        .await?;
        assert_eq!(
            rows.iter().map(|row| row.matched_text.as_str()).collect::<Vec<_>>(),
            vec!["guide-10.0.pdf", "guide-2.0.pdf"],
        );

        let wildcard_rows =
            repositories::content_metadata_search_repository::search_document_metadata_rows(
                &fixture.postgres,
                library.id,
                "%_",
                10,
            )
            .await?;
        assert!(
            wildcard_rows.is_empty(),
            "SQL wildcard characters in user input must remain literal"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
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

/// Owns the identifiers the knowledge search HTTP fixture seeds and returns.
#[derive(Clone, Copy)]
struct SearchSeedIds {
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    fact_id: Uuid,
    entity_id: Uuid,
    relation_id: Uuid,
    evidence_id: Uuid,
}

impl SearchSeedIds {
    fn new() -> Self {
        Self {
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_id: Uuid::now_v7(),
            fact_id: Uuid::now_v7(),
            entity_id: Uuid::now_v7(),
            relation_id: Uuid::now_v7(),
            evidence_id: Uuid::now_v7(),
        }
    }
}

/// Creates the temp database, migrates it, seeds the AI catalog and opens the
/// workspace/library the HTTP fixture writes into.
async fn bootstrap_knowledge_search_fixture() -> Result<(TempPostgresDatabase, AppState, Uuid, Uuid)>
{
    let mut settings =
        Settings::from_env().context("failed to load settings for knowledge search http test")?;
    let temp_postgres =
        TempPostgresDatabase::create(&settings.database_url, "knowledge_search_http").await?;
    settings.database_url = temp_postgres.database_url.clone();
    settings.credential_master_key = Some(test_credential_master_key());

    let postgres = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect to knowledge search postgres")?;
    sqlx::migrate!("./migrations")
        .run(&postgres)
        .await
        .context("failed to apply knowledge search migrations")?;
    seed_clean_v1_ai_catalog(&postgres)
        .await
        .context("failed to seed knowledge search HTTP AI catalog")?;
    postgres.close().await;

    let state = AppState::new(settings).await?;
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
    Ok((temp_postgres, state, workspace.id, library.id))
}

async fn seed_search_document_chunk(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    ids: &SearchSeedIds,
    now: DateTime<Utc>,
) -> Result<()> {
    let SearchSeedIds { document_id, revision_id, chunk_id, .. } = *ids;
    state
        .document_store
        .upsert_document(&KnowledgeDocumentRow {
            document_id,
            workspace_id,
            library_id,
            external_key: format!("search-document-{document_id}"),
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
            workspace_id,
            library_id,
            document_id,
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "upload".to_string(),
            storage_ref: Some("memory://knowledge-search".to_string()),
            source_uri: Some("memory://knowledge-search/source".to_string()),
            document_hint: None,
            mime_type: "text/plain".to_string(),
            checksum: crate::content_lifecycle_support::fixture_checksum(
                "knowledge-search-checksum",
            ),
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
            workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_index: 0,
            // Includes the endpoint literal so the lexical search for
            // `/orion/status` resolves this chunk-backed document (see
            // lexical_chunk_search_ignores_non_chunk_documents...).
            content_text: "orion lexical anchor /orion/status".to_string(),
            normalized_text: "orion lexical anchor /orion/status".to_string(),
            span_start: Some(0),
            span_end: Some(34),
            token_count: Some(4),
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

            occurred_at: None,
            occurred_until: None,
        })
        .await
        .context("failed to insert knowledge search chunk")?;
    seed_prepared_structured_revision(
        &PgDocumentStore { pool: state.persistence.postgres.clone() },
        PreparedStructuredRevisionSeed {
            workspace_id,
            library_id,
            document_id,
            revision_id,
            source_text: "orion lexical anchor /orion/status",
            chunk_count: 1,
        },
    )
    .await
    .context("failed to insert prepared knowledge search artifact")?;
    Ok(())
}

/// A knowledge-plane seed alone is not a published source. Search fixtures use
/// the same exact applied-owner/readable-head contract as ordinary readers.
async fn publish_search_source(pg: &PgPool, revision_id: Uuid, file_name: &str) -> Result<()> {
    let store = PgDocumentStore { pool: pg.clone() };
    let revision =
        store.get_revision(revision_id).await?.context("search fixture knowledge revision")?;
    let document = store
        .get_document(revision.document_id)
        .await?
        .context("search fixture knowledge document")?;
    anyhow::ensure!(
        document.workspace_id == revision.workspace_id
            && document.library_id == revision.library_id,
        "search fixture revision must match its document owner"
    );
    sqlx::query("insert into content_document (id,workspace_id,library_id,external_key,document_state,revision_sequence)
        values ($1,$2,$3,$4,'active',$5) on conflict (id) do nothing")
        .bind(revision.document_id).bind(revision.workspace_id).bind(revision.library_id)
        .bind(&document.external_key).bind(i32::try_from(revision.revision_number)?).execute(pg).await?;
    let owns_document: bool = sqlx::query_scalar(
        "select exists (select 1 from content_document
        where id=$1 and workspace_id=$2 and library_id=$3 and external_key=$4)",
    )
    .bind(revision.document_id)
    .bind(revision.workspace_id)
    .bind(revision.library_id)
    .bind(&document.external_key)
    .fetch_one(pg)
    .await?;
    anyhow::ensure!(owns_document, "search fixture cannot reuse a foreign content owner");
    let admission = crate::content_repository::imported_knowledge_revision_admission(
        pg,
        revision_id,
        file_name,
    )
    .await?;
    sqlx::query(
        "insert into content_revision (id,document_id,workspace_id,library_id,revision_number,
        content_source_kind,checksum,mime_type,byte_size,title,source_uri,storage_key,document_hint,admission_identity)
        values ($1,$2,$3,$4,$11,'upload',$5,$6,$7,$8,$9,$10,$12,$13) on conflict (id) do nothing",
    )
    .bind(revision_id)
    .bind(revision.document_id)
    .bind(revision.workspace_id)
    .bind(revision.library_id)
    .bind(&revision.checksum)
    .bind(&revision.mime_type)
    .bind(revision.byte_size)
    .bind(&revision.title)
    .bind(&revision.source_uri)
    .bind(&revision.storage_ref)
    .bind(i32::try_from(revision.revision_number)?)
    .bind(&revision.document_hint)
    .bind(admission)
    .execute(pg)
    .await?;
    let owns_revision: bool = sqlx::query_scalar(
        "select exists (select 1 from content_revision
        where id=$1 and document_id=$2 and workspace_id=$3 and library_id=$4
          and checksum=$5 and mime_type=$6 and byte_size=$7
          and title is not distinct from $8 and source_uri is not distinct from $9)",
    )
    .bind(revision_id)
    .bind(revision.document_id)
    .bind(revision.workspace_id)
    .bind(revision.library_id)
    .bind(&revision.checksum)
    .bind(&revision.mime_type)
    .bind(revision.byte_size)
    .bind(&revision.title)
    .bind(&revision.source_uri)
    .fetch_one(pg)
    .await?;
    anyhow::ensure!(owns_revision, "search fixture cannot rewrite an immutable source revision");
    sqlx::query(
        "update content_document set revision_sequence=greatest(revision_sequence,$2) where id=$1",
    )
    .bind(revision.document_id)
    .bind(i32::try_from(revision.revision_number)?)
    .execute(pg)
    .await?;
    // These frozen sources already belong to the fixture's seeded generation;
    // assembling their canonical ownership is not a second live publication.
    repositories::content_document_head_repository::upsert_document_head_without_generation_with_executor(
        pg,
        &repositories::content_document_head_repository::NewContentDocumentHead {
            document_id: revision.document_id,
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            publishing_mutation_item_id: None,
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    ).await?;
    anyhow::ensure!(
        repositories::content_fingerprint_repository::get_current_revision_id(
            pg,
            revision.document_id
        )
        .await?
            == Some(revision_id),
        "search fixture source must be canonically published"
    );
    Ok(())
}

/// Materializes exactly the readable heads declared by a frozen knowledge-plane
/// fixture before it exercises ordinary readers. Pending/historical revisions
/// are not promoted just because their rows exist.
async fn publish_declared_search_sources(pg: &PgPool, library_id: Uuid) -> Result<()> {
    let revisions: Vec<(Uuid, Option<String>)> = sqlx::query_as("select revision.revision_id, document.file_name
        from knowledge_document document join knowledge_revision revision
          on revision.revision_id=document.readable_revision_id
         and revision.document_id=document.document_id
         and revision.workspace_id=document.workspace_id and revision.library_id=document.library_id
        where document.library_id=$1 and document.document_state='active' and document.deleted_at is null
          and revision.text_state='text_readable' and revision.text_readable_at is not null")
        .bind(library_id).fetch_all(pg).await?;
    for (revision_id, file_name) in revisions {
        // The caller's fixture has no source filename unless its seed declares
        // one; this explicit fixture filename is not an application fallback.
        let file_name = file_name.as_deref().unwrap_or("search-source.txt");
        publish_search_source(pg, revision_id, file_name).await?;
    }
    Ok(())
}

async fn seed_entity_source_support(
    fixture: &KnowledgeSearchFixture,
    revision_id: Uuid,
    entity_id: Uuid,
) -> Result<()> {
    let revision = fixture
        .document_store
        .get_revision(revision_id)
        .await?
        .context("entity evidence must have its owning revision")?;
    anyhow::ensure!(
        repositories::content_fingerprint_repository::get_current_revision_id(
            &fixture.postgres,
            revision.document_id,
        )
        .await?
            == Some(revision_id),
        "entity evidence must belong to a canonical publication"
    );
    let now = Utc::now();
    fixture
        .graph_store
        .upsert_evidence_with_edges(
            &NewKnowledgeEvidence {
                evidence_id: Uuid::now_v7(),
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id,
                chunk_id: None,
                block_id: None,
                fact_id: None,
                span_start: None,
                span_end: None,
                quote_text: revision.normalized_text.context("published entity source text")?,
                literal_spans_json: json!([]),
                evidence_kind: "chunk_quote".to_owned(),
                extraction_method: "seed".to_owned(),
                confidence: Some(1.0),
                evidence_state: "active".to_owned(),
                freshness_generation: 1,
                created_at: Some(now),
                updated_at: Some(now),
            },
            EvidenceEdgeTargets {
                source_revision_id: Some(revision_id),
                supporting_entity_id: Some(entity_id),
                supporting_relation_id: None,
                supporting_fact_id: None,
                library_id: revision.library_id,
            },
        )
        .await?;
    Ok(())
}

async fn seed_search_technical_fact(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    ids: &SearchSeedIds,
    now: DateTime<Utc>,
) -> Result<()> {
    let SearchSeedIds { fact_id, chunk_id, document_id, revision_id, .. } = *ids;
    state
        .document_store
        .replace_technical_facts(
            revision_id,
            &[KnowledgeTechnicalFactRow {
                fact_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                fact_kind: "endpoint_path".to_string(),
                normalized_value_text: "/orion/status".to_string(),
                normalized_value_exact: "/orion/status".to_string(),
                normalized_value_json: json!({
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
    Ok(())
}

async fn seed_search_graph_anchors(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    ids: &SearchSeedIds,
    now: DateTime<Utc>,
) -> Result<()> {
    let SearchSeedIds { entity_id, relation_id, .. } = *ids;
    state
        .graph_store
        .upsert_entity(&NewKnowledgeEntity {
            entity_id,
            workspace_id,
            library_id,
            primary_label: "Orion Signal".to_string(),
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
                workspace_id,
                library_id,
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
            library_id,
        )
        .await
        .context("failed to insert knowledge search relation")?;
    state
        .graph_store
        .upsert_relation_subject_edge(relation_id, entity_id, library_id)
        .await
        .context("failed to link knowledge search relation subject")?;
    state
        .graph_store
        .upsert_relation_object_edge(relation_id, entity_id, library_id)
        .await
        .context("failed to link knowledge search relation object")?;
    Ok(())
}

async fn seed_search_vectors_and_evidence(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    ids: &SearchSeedIds,
    embedding_profile_key: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let SearchSeedIds {
        document_id,
        revision_id,
        chunk_id,
        fact_id,
        entity_id,
        relation_id,
        evidence_id,
    } = *ids;
    state
        .search_store
        .upsert_chunk_vector(&KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            chunk_id,
            revision_id,
            embedding_model_key: embedding_profile_key.to_string(),
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
            workspace_id,
            library_id,
            entity_id,
            embedding_model_key: embedding_profile_key.to_string(),
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
                workspace_id,
                library_id,
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
            EvidenceEdgeTargets {
                source_revision_id: Some(revision_id),
                supporting_entity_id: Some(entity_id),
                supporting_relation_id: Some(relation_id),
                supporting_fact_id: None,
                library_id,
            },
        )
        .await
        .context("failed to insert knowledge search evidence")?;
    Ok(())
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
    embedding_profile_key: String,
}

impl KnowledgeSearchHttpFixture {
    async fn create() -> Result<Self> {
        let (temp_postgres, mut state, workspace_id, library_id) =
            bootstrap_knowledge_search_fixture().await?;
        state.llm_gateway = Arc::new(FakeEmbeddingGateway { embedding: vec![0.9, 0.8, 0.7] });

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
        .find(|row| row.allowed_binding_purposes.iter().any(|purpose| purpose == "embed_chunk"))
        .context("expected seeded embedding model catalog row")?;
        let account_id = Uuid::now_v7();
        let encrypted_api_key = state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            "secret://knowledge-search/provider",
        )?;
        let credential = ai_repository::create_account(
            &state.persistence.postgres,
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "workspace",
                workspace_id: Some(workspace_id),
                library_id: None,
                provider_catalog_id: provider_catalog.id,
                label: "knowledge-search-provider-credential",
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&encrypted_api_key),
        )
        .await
        .context("failed to create knowledge search AI account")?;
        ai_repository::create_binding(
            &state.persistence.postgres,
            ai_repository::CreateAiBindingInput {
                scope_kind: "library",
                workspace_id: Some(workspace_id),
                library_id: Some(library_id),
                binding_purpose: "embed_chunk",
                account_id: credential.id,
                provider_catalog_id: provider_catalog.id,
                model_catalog_id: model_catalog.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: json!({}),
                binding_state: "active",
                updated_by_principal_id: None,
            },
        )
        .await
        .context("failed to create knowledge search library binding")?;
        let embedding_profile_key = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(&state, library_id, AiBindingPurpose::EmbedChunk)
            .await
            .context("failed to resolve knowledge search embedding binding")?
            .context("knowledge search embedding binding was not resolved")?
            .embedding_execution_profile_key();

        let token =
            mint_library_read_token(&state.persistence.postgres, workspace_id, library_id).await?;

        let ids = SearchSeedIds::new();
        let now = Utc::now();

        seed_search_document_chunk(&state, workspace_id, library_id, &ids, now).await?;
        seed_search_technical_fact(&state, workspace_id, library_id, &ids, now).await?;
        seed_search_graph_anchors(&state, workspace_id, library_id, &ids, now).await?;
        seed_search_vectors_and_evidence(
            &state,
            workspace_id,
            library_id,
            &ids,
            &embedding_profile_key,
            now,
        )
        .await?;

        Ok(Self {
            temp_postgres,
            state,
            token,
            workspace_id,
            library_id,
            document_id: ids.document_id,
            revision_id: ids.revision_id,
            chunk_id: ids.chunk_id,
            fact_id: ids.fact_id,
            entity_id: ids.entity_id,
            relation_id: ids.relation_id,
            embedding_profile_key,
        })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_postgres.drop().await
    }

    fn search_request(&self, query: &str, limit: usize) -> Result<Request<Body>> {
        let request_body = serde_json::to_vec(&json!({
            "text": query,
            "limit": limit,
            "chunkHitLimitPerDocument": 3,
            "evidenceSampleLimit": 2,
        }))
        .context("failed to encode knowledge search request body")?;
        Request::builder()
            .method("POST")
            .uri(format!("/v1/knowledge/libraries/{}/search", self.library_id))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(request_body))
            .context("failed to build knowledge search request")
    }

    async fn search_document_hit(&self, query: &str) -> Result<Value> {
        let response = self
            .app()
            .oneshot(self.search_request(query, 5)?)
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

    async fn seed_vector_only_source(&self) -> Result<SearchSeedIds> {
        let ids = SearchSeedIds::new();
        let now = Utc::now();
        seed_search_document_chunk(&self.state, self.workspace_id, self.library_id, &ids, now)
            .await?;
        let text = "neutral unrelated source";
        sqlx::query(
            "update knowledge_chunk set content_text=$2,normalized_text=$2,span_end=length($2)
            where chunk_id=$1",
        )
        .bind(ids.chunk_id)
        .bind(text)
        .execute(&self.state.persistence.postgres)
        .await?;
        sqlx::query("update knowledge_revision set normalized_text=$2 where revision_id=$1")
            .bind(ids.revision_id)
            .bind(text)
            .execute(&self.state.persistence.postgres)
            .await?;
        seed_prepared_structured_revision(
            &PgDocumentStore { pool: self.state.persistence.postgres.clone() },
            PreparedStructuredRevisionSeed {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                document_id: ids.document_id,
                revision_id: ids.revision_id,
                source_text: text,
                chunk_count: 1,
            },
        )
        .await?;
        self.state
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                chunk_id: ids.chunk_id,
                revision_id: ids.revision_id,
                embedding_model_key: self.embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_owned(),
                dimensions: 3,
                vector: vec![0.9, 0.8, 0.6],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        Ok(ids)
    }

    async fn wait_for_query_evidence_top_fact(
        &self,
        query: &str,
        literal: &str,
        expected_fact_id: Uuid,
    ) -> Result<ironrag_backend::services::query::search::QueryEvidenceSearchResult> {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        // These are exact-value-retrieval questions over a technical literal
        // (endpoint path, URL, HTTP method, parameter name) that the literal
        // appears verbatim within (`LiteralSpan::text` is always an exact
        // substring of the question). The IR mirrors that so
        // `search_query_evidence` sets `exact_literal_bias`
        // (`is_exact_literal_technical` = RetrieveValue + an exact-shaped literal).
        let descriptive_ir = ironrag_backend::domains::query_ir::QueryIR {
            act: ironrag_backend::domains::query_ir::QueryAct::RetrieveValue,
            scope: ironrag_backend::domains::query_ir::QueryScope::SingleDocument,
            language: ironrag_backend::domains::query_ir::QueryLanguage::Auto,
            retrieval_query: None,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: vec![ironrag_backend::domains::query_ir::LiteralSpan {
                text: literal.to_string(),
                kind: ironrag_backend::domains::query_ir::LiteralKind::infer(literal),
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 1.0,
        };
        loop {
            // Exercise the production-shaped call: the raw natural-language
            // question ("HTTP method GET"), not the pre-extracted literal
            // ("GET"). `search_query_evidence` must derive the exact-literal
            // fact-search string from `query_ir.literal_constraints` itself —
            // production callers only ever have the raw question in hand.
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
    let plaintext = mint_plaintext_api_token();
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
        &iam_repository::NewGrant {
            principal_id: token.principal_id,
            resource_kind: "library",
            resource_id: library_id,
            permission_kind: PERMISSION_LIBRARY_READ,
            granted_by_principal_id: None,
            expires_at: None,
        },
    )
    .await
    .context("failed to create knowledge search grant")?;
    Ok(plaintext)
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn search_denies_withdrawn_vector_source_outside_document_page() -> Result<()> {
    use ironrag_backend::infra::repositories::admission_repository::{
        VisibilityAdmissionRequest, admit_visibility_intent_with_failpoint,
    };
    use ironrag_contracts::documents::{CreateVisibilityIntentRequest, ReplacementIdentity};

    let fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
    let result = Box::pin(async {
        let b = fixture.seed_vector_only_source().await?;
        let control = fixture.app().oneshot(fixture.search_request("orion", 1)?).await?;
        assert_eq!(control.status(), StatusCode::OK);
        let control: Value =
            serde_json::from_slice(&axum::body::to_bytes(control.into_body(), 256 * 1024).await?)?;
        let documents = control["documentHits"].as_array().context("document page")?;
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0]["document"]["documentId"], json!(fixture.document_id));
        let vectors = control["vectorChunkHits"].as_array().context("independent vector hits")?;
        assert!(
            vectors.iter().any(|hit| hit["chunkId"] == json!(b.chunk_id)
                && hit["revisionId"] == json!(b.revision_id)),
            "positive control must retain B's vector hit outside X's document page"
        );

        let (reached, release) = fixture
            .state
            .runtime_graph_projection_cache
            .pause_next_search_read(fixture.library_id)
            .await;
        let app = fixture.app();
        let request = fixture.search_request("orion", 1)?;
        let mut reader =
            ironrag_backend::shared::cancellation::AbortOnDrop::new(tokio::spawn(async move {
                app.oneshot(request).await
            }));
        timeout(Duration::from_secs(10), reached)
            .await
            .context("actual search handler did not reach the hydrated-response gate")??;
        admit_visibility_intent_with_failpoint(
            &fixture.state.persistence.postgres,
            &VisibilityAdmissionRequest {
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                document_id: b.document_id,
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                intent: CreateVisibilityIntentRequest {
                    idempotency_key: "off-page-vector-withdrawal".to_owned(),
                    source_identity: "off-page-vector-source".to_owned(),
                    materialization_token: None,
                    replacement: ReplacementIdentity {
                        file_name: "replacement.txt".to_owned(),
                        checksum: format!("sha256:{}", "b".repeat(64)),
                        mime_type: "text/plain".to_owned(),
                        byte_size: 24,
                        title: Some("Replacement source".to_owned()),
                        language_code: None,
                        source_uri: None,
                        document_hint: None,
                    },
                },
            },
            None,
        )
        .await?;
        let pg = &fixture.state.persistence.postgres;
        assert!(
            repositories::content_fingerprint_repository::get_current_revision_id(
                pg,
                b.document_id
            )
            .await?
            .is_none()
        );
        assert_eq!(
            repositories::content_fingerprint_repository::get_current_revision_id(
                pg,
                fixture.document_id
            )
            .await?,
            Some(fixture.revision_id),
            "the ranked page's X source must remain stable"
        );
        release.send(()).map_err(|()| anyhow!("hydrated search reader disappeared"))?;
        let response =
            timeout(Duration::from_secs(10), reader.handle_mut().context("search reader handle")?)
                .await
                .context("search handler did not finish after gate release")???;
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "withdrawn off-page vector source must deny the actual HTTP search response"
        );
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024).await?;
        assert!(!std::str::from_utf8(&body)?.contains(&b.chunk_id.to_string()));
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires isolated postgres and redis services"]
async fn retained_chunk_source_membership_requires_exact_scope_owner_and_revision() -> Result<()> {
    use repositories::content_fingerprint_repository::{
        SelectedChunkSource, chunk_sources_are_current,
    };
    let fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let source = SelectedChunkSource {
            chunk_id: fixture.chunk_id,
            revision_id: fixture.revision_id,
            library_id: fixture.library_id,
        };
        assert!(
            chunk_sources_are_current(pg, fixture.workspace_id, fixture.library_id, &[source])
                .await?
        );
        assert!(
            !chunk_sources_are_current(
                pg,
                fixture.workspace_id,
                fixture.library_id,
                &[source, SelectedChunkSource { chunk_id: Uuid::now_v7(), ..source }]
            )
            .await?,
            "one missing source must deny the retained batch"
        );
        assert!(
            !chunk_sources_are_current(pg, Uuid::now_v7(), fixture.library_id, &[source]).await?,
            "a current source in a foreign workspace must not be approved"
        );
        let foreign = repositories::catalog_repository::create_library(
            pg,
            fixture.workspace_id,
            &format!("foreign-search-{}", Uuid::now_v7().simple()),
            "Foreign search source",
            None,
            None,
        )
        .await?;
        let foreign_ids = SearchSeedIds::new();
        seed_search_document_chunk(
            &fixture.state,
            fixture.workspace_id,
            foreign.id,
            &foreign_ids,
            Utc::now(),
        )
        .await?;
        let foreign_source = SelectedChunkSource {
            chunk_id: foreign_ids.chunk_id,
            revision_id: foreign_ids.revision_id,
            library_id: foreign.id,
        };
        assert!(
            chunk_sources_are_current(pg, fixture.workspace_id, foreign.id, &[foreign_source])
                .await?
        );
        assert!(
            !chunk_sources_are_current(
                pg,
                fixture.workspace_id,
                fixture.library_id,
                &[foreign_source]
            )
            .await?,
            "a genuinely published foreign-library chunk must be denied"
        );
        assert!(
            !chunk_sources_are_current(
                pg,
                fixture.workspace_id,
                fixture.library_id,
                &[SelectedChunkSource { revision_id: foreign_ids.revision_id, ..source }]
            )
            .await?,
            "the selected revision must be the chunk's exact revision, not any current revision"
        );
        assert!(
            !chunk_sources_are_current(
                pg,
                fixture.workspace_id,
                fixture.library_id,
                &[SelectedChunkSource { library_id: foreign.id, ..source }]
            )
            .await?,
            "the retained hit's wire library must match the requested source scope"
        );
        sqlx::query(
            "update content_document set document_state='deleted',deleted_at=now() where id=$1",
        )
        .bind(fixture.document_id)
        .execute(pg)
        .await?;
        assert!(
            !chunk_sources_are_current(pg, fixture.workspace_id, fixture.library_id, &[source])
                .await?,
            "a deleted owning document must revoke retained chunk membership"
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with database create/drop access"]
async fn library_generation_signals_count_authoritative_chunk_embedding_vectors() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "vector-roundtrip").await?;
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let embedding_profile_key = synthetic_embedding_profile_key(model_catalog_id);
        let now = Utc::now();

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "generation-signal".to_string(),
                file_name: None,
                title: Some("Generation Signal Fixture".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(2),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert generation signal document")?;

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
                checksum: crate::content_lifecycle_support::fixture_checksum("revision"),
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
            .context("failed to insert authoritative chunk embedding vector")?;

        publish_declared_search_sources(&fixture.postgres, library_id).await?;
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
async fn vector_ready_revisions_missing_chunk_vectors_are_listed_by_document() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "missing-chunk-vectors").await?;
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let no_chunk_revision_id = Uuid::now_v7();
        let pending_revision_id = Uuid::now_v7();
        let pending_chunk_id = Uuid::now_v7();
        let superseded_revision_id = Uuid::now_v7();
        let superseded_chunk_id = Uuid::now_v7();
        let other_library_id =
            create_vector_test_library_in_workspace(&fixture, workspace_id, "other-inventory")
                .await?;
        let other_revision_id = Uuid::now_v7();
        let other_chunk_id = Uuid::now_v7();
        let no_chunk_document_id = Uuid::now_v7();
        let pending_document_id = Uuid::now_v7();
        let superseded_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let replaced_document_id = Uuid::now_v7();
        let replaced_head_revision_id = Uuid::now_v7();
        let replaced_predecessor_revision_id = Uuid::now_v7();
        let replaced_predecessor_chunk_id = Uuid::now_v7();
        let replaced_head_chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let embedding_profile_key = synthetic_embedding_profile_key(model_catalog_id);
        let now = Utc::now();

        // Every revision and chunk points at a logical document, so seed the
        // parent `knowledge_document` rows before the revisions to satisfy the
        // `knowledge_revision_document_id_fkey` / chunk foreign keys.
        for (document_uuid, library, revision, external_key) in [
            (document_id, library_id, revision_id, "vector-inventory-main"),
            (no_chunk_document_id, library_id, no_chunk_revision_id, "vector-inventory-no-chunk"),
            (pending_document_id, library_id, pending_revision_id, "vector-inventory-pending"),
            (
                superseded_document_id,
                library_id,
                superseded_revision_id,
                "vector-inventory-superseded",
            ),
            (other_document_id, other_library_id, other_revision_id, "vector-inventory-other"),
            (
                replaced_document_id,
                library_id,
                replaced_head_revision_id,
                "vector-inventory-replaced",
            ),
        ] {
            fixture
                .document_store
                .upsert_document(&KnowledgeDocumentRow {
                    document_id: document_uuid,
                    workspace_id,
                    library_id: library,
                    external_key: external_key.to_string(),
                    file_name: None,
                    title: None,
                    source_uri: None,
                    document_hint: None,
                    document_state: "active".to_string(),
                    active_revision_id: Some(revision),
                    readable_revision_id: Some(revision),
                    latest_revision_no: Some(1),
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                    parent_document_id: None,
                    document_role: "primary".to_string(),
                })
                .await
                .context("failed to insert vector inventory document")?;
        }

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
                checksum: crate::content_lifecycle_support::fixture_checksum("revision"),
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
                document_id: no_chunk_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum("no-chunk-revision"),
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
                document_id: pending_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum("pending-revision"),
                title: Some("Pending Revision".to_string()),
                byte_size: 1,
                normalized_text: Some("pending".to_string()),
                text_checksum: Some("pending".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
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
                document_id: pending_document_id,
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
                document_id: superseded_document_id,
                revision_number: 1,
                revision_state: "superseded".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum("superseded-revision"),
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
                document_id: superseded_document_id,
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
                document_id: other_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum("other-revision"),
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
                document_id: other_document_id,
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
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert other-library chunk")?;

        // A replaced document: its retired predecessor revision still reads
        // `vector_state = 'ready'` with `superseded_by_revision_id IS NULL`, but
        // the document now serves a different head revision. Only the live head
        // may be reported; the predecessor has no operator-visible document.
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: replaced_predecessor_revision_id,
                workspace_id,
                library_id,
                document_id: replaced_document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum(
                    "replaced-predecessor-revision",
                ),
                title: Some("Replaced Predecessor".to_string()),
                byte_size: 1,
                normalized_text: Some("replaced predecessor".to_string()),
                text_checksum: Some("replaced-predecessor".to_string()),
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
            .context("failed to insert replaced predecessor revision")?;
        seed_vector_inventory_chunk(
            &fixture,
            VectorInventoryChunkSeed {
                workspace_id,
                library_id,
                document_id: replaced_document_id,
                revision_id: replaced_predecessor_revision_id,
                chunk_id: replaced_predecessor_chunk_id,
                content: "replaced predecessor",
            },
        )
        .await?;
        fixture
            .document_store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id: replaced_head_revision_id,
                workspace_id,
                library_id,
                document_id: replaced_document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "replace".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum(
                    "replaced-head-revision",
                ),
                title: Some("Replaced Head".to_string()),
                byte_size: 1,
                normalized_text: Some("replaced head".to_string()),
                text_checksum: Some("replaced-head".to_string()),
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
            .context("failed to insert replaced head revision")?;
        seed_vector_inventory_chunk(
            &fixture,
            VectorInventoryChunkSeed {
                workspace_id,
                library_id,
                document_id: replaced_document_id,
                revision_id: replaced_head_revision_id,
                chunk_id: replaced_head_chunk_id,
                content: "replaced head",
            },
        )
        .await?;
        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                chunk_id: replaced_head_chunk_id,
                revision_id: replaced_head_revision_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![0.1, 0.3, 0.5],
                freshness_generation: 1,
                created_at: now,
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert replaced head vector")?;

        // The documents-list filter is sent this exact set, so the assertion is
        // on the set and not on its size: a right-sized wrong set would send an
        // operator to somebody else's document.
        publish_declared_search_sources(&fixture.postgres, library_id).await?;
        let stale_documents = fixture
            .document_store
            .list_documents_missing_chunk_vectors(library_id)
            .await
            .context("failed to list vector inventory mismatch")?;
        assert_eq!(stale_documents, vec![document_id]);

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
        let repaired_documents = fixture
            .document_store
            .list_documents_missing_chunk_vectors(library_id)
            .await
            .context("failed to list repaired vector inventory")?;
        assert!(
            repaired_documents.is_empty(),
            "repaired library still reports a gap: {repaired_documents:?}"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
async fn lexical_chunk_search_stays_library_scoped() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, target_library_id) =
            create_empty_vector_test_library(&fixture, "lexical-target").await?;
        let distractor_library_id =
            create_vector_test_library_in_workspace(&fixture, workspace_id, "lexical-distractor")
                .await?;
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
                checksum: crate::content_lifecycle_support::fixture_checksum("target-checksum"),
                title: Some("Target".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("target-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
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

                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert target chunk")?;
        seed_prepared_structured_revision(
            &fixture.document_store,
            PreparedStructuredRevisionSeed {
                workspace_id,
                library_id: target_library_id,
                document_id: target_document_id,
                revision_id: target_revision_id,
                source_text: "orion lexical anchor",
                chunk_count: 1,
            },
        )
        .await
        .context("failed to insert prepared target artifact")?;

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
                checksum: crate::content_lifecycle_support::fixture_checksum("distractor-checksum"),
                title: Some("Distractor".to_string()),
                byte_size: 32,
                normalized_text: Some("orion lexical anchor".to_string()),
                text_checksum: Some("distractor-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
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

                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert distractor chunk")?;
        seed_prepared_structured_revision(
            &fixture.document_store,
            PreparedStructuredRevisionSeed {
                workspace_id,
                library_id: distractor_library_id,
                document_id: distractor_document_id,
                revision_id: distractor_revision_id,
                source_text: "orion lexical anchor",
                chunk_count: 1,
            },
        )
        .await
        .context("failed to insert prepared distractor artifact")?;

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
#[ignore = "requires local postgres service with required extensions"]
async fn lexical_chunk_search_hydrates_only_the_temporally_matching_readable_revision() -> Result<()>
{
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "temporal-hydration").await?;
        let document_id = Uuid::now_v7();
        let stale_revision_id = Uuid::now_v7();
        let current_revision_id = Uuid::now_v7();
        let stale_chunk_id = Uuid::now_v7();
        let current_chunk_id = Uuid::now_v7();
        let now = Utc::now();
        let occurred_at = now - chrono::TimeDelta::minutes(5);
        let token = "AlphaLedger_7";
        let current_content = "AlphaLedger_7 current ledger entry";
        let stale_content = "AlphaLedger_7 stale ledger entry";
        let current_section = vec!["current-section".to_string()];
        let current_heading = vec!["Current Heading".to_string()];
        let current_quality = 0.625_f32;

        fixture
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: "revision-first-lexical".to_string(),
                file_name: None,
                title: Some(token.to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(current_revision_id),
                readable_revision_id: Some(current_revision_id),
                latest_revision_no: Some(2),
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .context("failed to insert revision-first lexical document")?;

        for revision in [
            KnowledgeRevisionRow {
                revision_id: stale_revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "superseded".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision-first/stale".to_string()),
                source_uri: Some("memory://revision-first/stale/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum(
                    "revision-first-stale-checksum",
                ),
                title: Some(token.to_string()),
                byte_size: i64::try_from(stale_content.len())?,
                normalized_text: Some(stale_content.to_string()),
                text_checksum: Some("revision-first-stale-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now - chrono::TimeDelta::seconds(1)),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now - chrono::TimeDelta::seconds(1),
            },
            KnowledgeRevisionRow {
                revision_id: current_revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 2,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some("memory://revision-first/current".to_string()),
                source_uri: Some("memory://revision-first/current/source".to_string()),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: crate::content_lifecycle_support::fixture_checksum(
                    "revision-first-current-checksum",
                ),
                title: Some(token.to_string()),
                byte_size: i64::try_from(current_content.len())?,
                normalized_text: Some(current_content.to_string()),
                text_checksum: Some("revision-first-current-text-checksum".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            },
        ] {
            fixture
                .document_store
                .upsert_revision(&revision)
                .await
                .context("failed to insert revision-first lexical revision")?;
        }

        for chunk in [
            KnowledgeChunkRow {
                chunk_id: stale_chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: stale_revision_id,
                chunk_index: 0,
                content_text: stale_content.to_string(),
                normalized_text: stale_content.to_string(),
                span_start: Some(0),
                span_end: Some(i32::try_from(stale_content.chars().count())?),
                token_count: Some(4),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: vec!["stale-section".to_string()],
                heading_trail: vec!["Stale Heading".to_string()],
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
                quality_score: Some(1.0),
                window_text: None,
                occurred_at: Some(occurred_at),
                occurred_until: None,
            },
            KnowledgeChunkRow {
                chunk_id: current_chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id: current_revision_id,
                chunk_index: 0,
                content_text: current_content.to_string(),
                normalized_text: current_content.to_string(),
                span_start: Some(0),
                span_end: Some(i32::try_from(current_content.chars().count())?),
                token_count: Some(4),
                chunk_kind: Some("paragraph".to_string()),
                support_block_ids: Vec::new(),
                section_path: current_section.clone(),
                heading_trail: current_heading.clone(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(2),
                vector_generation: None,
                quality_score: Some(current_quality),
                window_text: None,
                occurred_at: Some(occurred_at),
                occurred_until: None,
            },
        ] {
            fixture
                .document_store
                .upsert_chunk(&chunk)
                .await
                .context("failed to insert revision-first lexical chunk")?;
        }

        for (revision_id, content) in
            [(stale_revision_id, stale_content), (current_revision_id, current_content)]
        {
            seed_prepared_structured_revision(
                &fixture.document_store,
                PreparedStructuredRevisionSeed {
                    workspace_id,
                    library_id,
                    document_id,
                    revision_id,
                    source_text: content,
                    chunk_count: 1,
                },
            )
            .await
            .context("failed to insert prepared revision-first lexical artifact")?;
        }

        let hits = fixture
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: token,
                query_origin: LexicalQueryOrigin::UserQuery,
                limit: 4,
                temporal_start: Some(occurred_at),
                temporal_end: Some(occurred_at),
                text_search_config: None,
            })
            .await
            .context("failed to search the current revision at its temporal boundary")?;
        anyhow::ensure!(
            hits.len() == 2,
            "the text and title-identity lanes must both survive; got {} rows",
            hits.len()
        );
        anyhow::ensure!(hits[0].chunk_id == current_chunk_id);
        anyhow::ensure!(hits[1].chunk_id == current_chunk_id);
        anyhow::ensure!(hits[0].score > hits[1].score);
        anyhow::ensure!(!hits.iter().any(|hit| hit.chunk_id == stale_chunk_id));
        for hit in &hits {
            anyhow::ensure!(hit.workspace_id == workspace_id);
            anyhow::ensure!(hit.library_id == library_id);
            anyhow::ensure!(hit.revision_id == current_revision_id);
            anyhow::ensure!(hit.content_text == current_content);
            anyhow::ensure!(hit.normalized_text == current_content);
            anyhow::ensure!(hit.section_path.as_slice() == current_section.as_slice());
            anyhow::ensure!(hit.heading_trail.as_slice() == current_heading.as_slice());
            anyhow::ensure!(hit.quality_score == Some(current_quality));
            anyhow::ensure!(hit.exact_identifier_terms == vec![token.to_string()]);
        }

        let after_hits = fixture
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: token,
                query_origin: LexicalQueryOrigin::UserQuery,
                limit: 4,
                temporal_start: Some(occurred_at + chrono::TimeDelta::microseconds(1)),
                temporal_end: None,
                text_search_config: None,
            })
            .await
            .context("failed to search strictly after the current chunk timestamp")?;
        anyhow::ensure!(after_hits.is_empty());
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
    let result =
        async {
            let (workspace_id, library_id) =
                create_empty_vector_test_library(&fixture, "revision-replacement").await?;
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
                    checksum: crate::content_lifecycle_support::fixture_checksum("vector-checksum"),
                    title: Some("Vector Doc".to_string()),
                    byte_size: 32,
                    normalized_text: Some("vector generation anchor".to_string()),
                    text_checksum: Some("vector-text-checksum".to_string()),
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
                    primary_label: "VectorEntity".to_string(),
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

            publish_search_source(&fixture.postgres, revision_id, "vector-source.txt").await?;
            seed_entity_source_support(&fixture, revision_id, entity_id).await?;

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

            let entity_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&fixture.temp_database.database_url)
                .await?;
            sqlx::query("select '[1,0,0]'::vector").execute(&entity_pool).await?;
            sqlx::query("set plan_cache_mode = force_generic_plan").execute(&entity_pool).await?;
            let session_settings =
                "select current_setting('plan_cache_mode'), current_setting('hnsw.ef_search'),
            current_setting('hnsw.iterative_scan'), current_setting('hnsw.max_scan_tuples'),
            current_setting('hnsw.scan_mem_multiplier')";
            let before: (String, String, String, String, String) =
                sqlx::query_as(session_settings).fetch_one(&entity_pool).await?;
            let entity_store = PgSearchStore { pool: entity_pool.clone() };
            let current_entity_hits = entity_store
                .search_entity_vectors_by_similarity(
                    VectorSimilaritySearchInput {
                        dim: 3,
                        library_id,
                        embedding_model_key: &embedding_profile_key,
                        query_vector: &[2.0, 2.0, 2.0],
                        n_probe: None,
                    },
                    8,
                )
                .await
                .context("failed to search current-generation entity vectors")?;
            assert_eq!(current_entity_hits.len(), 1);
            assert_eq!(current_entity_hits[0].entity_id, entity_id);
            assert_eq!(current_entity_hits[0].freshness_generation, 2);
            let after: (String, String, String, String, String) =
                sqlx::query_as(session_settings).fetch_one(&entity_pool).await?;
            assert_eq!(after, before);
            let (generic, custom): (i64, i64) = sqlx::query_as(
            "select coalesce(sum(generic_plans),0)::bigint, coalesce(sum(custom_plans),0)::bigint
             from pg_prepared_statements where position('knowledge_entity_vector_d3' in statement)>0
             and position('limit $5' in statement)>0",
        ).fetch_one(&entity_pool).await?;
            assert_eq!(generic, 2, "SQLx describe and entity execution retain the generic policy");
            assert_eq!(custom, 0);
            entity_pool.close().await;

            let mut delete_transaction = fixture.postgres.begin().await?;
            let deleted = sqlx::query(
                "delete from knowledge_entity_vector_d3
             where library_id = $1
               and entity_id = $2
               and embedding_model_key = $3
               and freshness_generation = 2",
            )
            .bind(library_id)
            .bind(entity_id)
            .bind(&embedding_profile_key)
            .execute(&mut *delete_transaction)
            .await?
            .rows_affected();
            assert_eq!(deleted, 1);
            let decremented = sqlx::query(
                "update knowledge_vector_relation_manifest
             set row_count = row_count - 1
             where library_id = $1
               and relation_name = 'knowledge_entity_vector_d3'
               and embedding_model_key = $2
               and vector_kind = 'entity_embedding'
               and row_count >= 1",
            )
            .bind(library_id)
            .bind(&embedding_profile_key)
            .execute(&mut *delete_transaction)
            .await?
            .rows_affected();
            assert_eq!(decremented, 1);
            delete_transaction.commit().await?;
            let stale_only_entity_hits = fixture
                .search_store
                .search_entity_vectors_by_similarity(
                    VectorSimilaritySearchInput {
                        dim: 3,
                        library_id,
                        embedding_model_key: &embedding_profile_key,
                        query_vector: &[1.0, 1.0, 1.0],
                        n_probe: None,
                    },
                    8,
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
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "generation-signals").await?;
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
                checksum: crate::content_lifecycle_support::fixture_checksum("revision-one"),
                title: Some("Revision One".to_string()),
                byte_size: 32,
                normalized_text: Some("obsolete nebula anchor".to_string()),
                text_checksum: Some("replacement-text-checksum-1".to_string()),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
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

                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert revision one chunk")?;
        seed_prepared_structured_revision(
            &fixture.document_store,
            PreparedStructuredRevisionSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_one_id,
                source_text: "obsolete nebula anchor",
                chunk_count: 1,
            },
        )
        .await
        .context("failed to insert prepared revision-one artifact")?;
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
                checksum: crate::content_lifecycle_support::fixture_checksum("revision-two"),
                title: Some("Revision Two".to_string()),
                byte_size: 32,
                normalized_text: Some("fresh pulsar signal".to_string()),
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
            // Supersession is recorded via `superseded_by_revision_id`; the
            // authoritative projection states are preserved (mirrors the production
            // supersede path in graph/service.rs). "superseded" is not a valid
            // projection state in the fresh baseline.
            .update_revision_readiness(RevisionReadinessUpdate {
                revision_id: revision_one_id,
                text_state: "text_readable",
                vector_state: "accepted",
                graph_state: "accepted",
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: Some(revision_two_id),
            })
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
                // Lexically disjoint from revision one's "obsolete nebula anchor"
                // so searching the old text after replacement returns nothing
                // (a shared term would relax-match this replacement chunk).
                content_text: "fresh pulsar signal".to_string(),
                normalized_text: "fresh pulsar signal".to_string(),
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

                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to insert revision two chunk")?;
        seed_prepared_structured_revision(
            &fixture.document_store,
            PreparedStructuredRevisionSeed {
                workspace_id,
                library_id,
                document_id,
                revision_id: revision_two_id,
                source_text: "fresh pulsar signal",
                chunk_count: 1,
            },
        )
        .await
        .context("failed to insert prepared revision-two artifact")?;
        fixture
            .document_store
            .update_document_pointers(DocumentPointersUpdate {
                document_id,
                document_state: "active",
                active_revision_id: Some(revision_two_id),
                readable_revision_id: Some(revision_two_id),
                latest_revision_no: Some(2),
                title: None,
                deleted_at: None,
            })
            .await
            .context("failed to update document pointers after replacement")?
            .ok_or_else(|| anyhow!("document disappeared during pointer update"))?;
        fixture.wait_for_chunk_hits(library_id, "obsolete nebula anchor", &[]).await?;
        let new_hits =
            fixture.wait_for_chunk_hits(library_id, "fresh pulsar signal", &[chunk_two_id]).await?;
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
        assert_eq!(revision_one.text_state, "text_readable");

        let revision_two = fixture
            .document_store
            .get_revision(revision_two_id)
            .await
            .context("failed to reload revision two")?
            .ok_or_else(|| anyhow!("revision two not found"))?;
        assert_eq!(revision_two.vector_state, "ready");
        assert_eq!(revision_two.graph_state, "ready");
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
    let fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;

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
        assert!(document_hit["document"].get("workspace_id").is_none());
        assert!(document_hit["document"].get("workspaceId").is_none());
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
        assert_eq!(entity_hits[0]["\u{63}anonicalLabel"], json!("Orion Signal"));
        assert!(entity_hits[0].get("workspace_id").is_none());

        let relation_hits =
            body["relationHits"].as_array().context("relationHits must be an array")?;
        assert_eq!(relation_hits.len(), 1);
        assert_eq!(relation_hits[0]["relationId"], json!(fixture.relation_id));
        assert_eq!(relation_hits[0]["predicate"], json!("Orion relation"));
        assert!(relation_hits[0].get("workspace_id").is_none());

        let vector_chunk_hits =
            body["vectorChunkHits"].as_array().context("vectorChunkHits must be an array")?;
        assert_eq!(vector_chunk_hits.len(), 1);
        assert_eq!(vector_chunk_hits[0]["chunkId"], json!(fixture.chunk_id));
        assert!(vector_chunk_hits[0].get("workspace_id").is_none());

        let vector_entity_hits =
            body["vectorEntityHits"].as_array().context("vectorEntityHits must be an array")?;
        assert_eq!(vector_entity_hits.len(), 1);
        assert_eq!(vector_entity_hits[0]["entityId"], json!(fixture.entity_id));
        assert!(vector_entity_hits[0].get("workspace_id").is_none());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn lexical_chunk_search_ignores_non_chunk_documents_in_shared_search_view() -> Result<()> {
    let fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;

    let result = async {
        let deadline = Instant::now() + SEARCH_WAIT_TIMEOUT;
        loop {
            let hits = fixture
                .state
                .search_store
                .search_chunks(ChunkLexicalQuery {
                    library_id: fixture.library_id,
                    query: "/orion/status",
                    query_origin: LexicalQueryOrigin::UserQuery,
                    limit: 8,
                    temporal_start: None,
                    temporal_end: None,
                    text_search_config: None,
                })
                .await
                .context("failed to run lexical chunk search against shared search view")?;
            let chunk_ids = hits.iter().map(|row| row.chunk_id).collect::<BTreeSet<_>>();
            if chunk_ids == BTreeSet::from([fixture.chunk_id]) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "timed out waiting for lexical chunk search to return only the authoritative chunk {}; last observed {:?}",
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
    let fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;

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
                        normalized_value_text: "/orion/status".to_string(),
                        normalized_value_exact: "/orion/status".to_string(),
                        normalized_value_json: json!({ "value_type": "text", "value": "/orion/status" }),
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
                        normalized_value_text: "https://api.example.com/orion/status".to_string(),
                        normalized_value_exact: "https://api.example.com/orion/status".to_string(),
                        normalized_value_json: json!({
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
                        normalized_value_text: "GET".to_string(),
                        normalized_value_exact: "GET".to_string(),
                        normalized_value_json: json!({ "value_type": "text", "value": "GET" }),
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
                        normalized_value_text: "pageNumber".to_string(),
                        normalized_value_exact: "pageNumber".to_string(),
                        normalized_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
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
                        normalized_value_text: "pageSize".to_string(),
                        normalized_value_exact: "pageSize".to_string(),
                        normalized_value_json: json!({ "value_type": "text", "value": "pageSize" }),
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
            .context("failed to reseed authoritative technical facts for ranking regression")?;

        let endpoint_result = fixture
            .wait_for_query_evidence_top_fact("/orion/status", "/orion/status", fixture.fact_id)
            .await?;
        assert!(endpoint_result.exact_literal_bias);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_id, fixture.fact_id);
        assert_eq!(endpoint_result.technical_fact_hits[0].fact_kind, "endpoint_path");
        assert!(endpoint_result.technical_fact_hits[0].exact_match);

        let url_result = fixture
            .wait_for_query_evidence_top_fact(
                "https://api.example.com/orion/status",
                "https://api.example.com/orion/status",
                url_fact_id,
            )
            .await?;
        assert!(url_result.exact_literal_bias);
        assert_eq!(url_result.technical_fact_hits[0].fact_id, url_fact_id);
        assert_eq!(url_result.technical_fact_hits[0].fact_kind, "url");
        assert!(url_result.technical_fact_hits[0].exact_match);

        let method_result = fixture
            .wait_for_query_evidence_top_fact("HTTP method GET", "GET", method_fact_id)
            .await?;
        assert!(method_result.exact_literal_bias);
        assert_eq!(method_result.technical_fact_hits[0].fact_id, method_fact_id);
        assert_eq!(method_result.technical_fact_hits[0].fact_kind, "http_method");

        let parameter_result = fixture
            .wait_for_query_evidence_top_fact("query parameter pageNumber", "pageNumber", parameter_fact_id)
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
#[ignore = "requires local postgres service with required extensions"]
async fn test_two_libraries_different_dims_isolated() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_a_id) =
            create_empty_vector_test_library(&fixture, "dims-library-a").await?;
        let library_b_id =
            create_vector_test_library_in_workspace(&fixture, workspace_id, "dims-library-b")
                .await?;
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

        // `search_chunk_vectors_by_similarity` only surfaces vectors backed by a
        // ready chunk whose document is active and points its readable revision
        // at the vector's revision. Seed that document -> revision -> chunk chain
        // per library so each shard's own vector is retrievable.
        let document_a_id = Uuid::now_v7();
        let document_b_id = Uuid::now_v7();
        for (shard_library_id, document_id, revision_id, chunk_id, label) in [
            (library_a_id, document_a_id, revision_a_id, chunk_a_id, "library-a"),
            (library_b_id, document_b_id, revision_b_id, chunk_b_id, "library-b"),
        ] {
            fixture
                .document_store
                .upsert_document(&KnowledgeDocumentRow {
                    document_id,
                    workspace_id,
                    library_id: shard_library_id,
                    external_key: format!("two-libraries-{label}"),
                    file_name: None,
                    title: Some("Two Libraries Isolation".to_string()),
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
                .with_context(|| format!("failed to insert {label} document"))?;
            fixture
                .document_store
                .upsert_revision(&KnowledgeRevisionRow {
                    revision_id,
                    workspace_id,
                    library_id: shard_library_id,
                    document_id,
                    revision_number: 1,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: Some(format!("memory://{label}")),
                    source_uri: Some(format!("memory://{label}/source")),
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: crate::content_lifecycle_support::fixture_checksum(&format!(
                        "{label}-checksum"
                    )),
                    title: Some("Two Libraries Isolation".to_string()),
                    byte_size: 16,
                    normalized_text: Some("isolation anchor".to_string()),
                    text_checksum: Some(format!("{label}-text-checksum")),
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
                .with_context(|| format!("failed to insert {label} revision"))?;
            fixture
                .document_store
                .upsert_chunk(&KnowledgeChunkRow {
                    chunk_id,
                    workspace_id,
                    library_id: shard_library_id,
                    document_id,
                    revision_id,
                    chunk_index: 0,
                    content_text: "isolation anchor".to_string(),
                    normalized_text: "isolation anchor".to_string(),
                    span_start: Some(0),
                    span_end: Some(16),
                    token_count: Some(2),
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
                    occurred_at: None,
                    occurred_until: None,
                })
                .await
                .with_context(|| format!("failed to insert {label} chunk"))?;
            seed_prepared_structured_revision(
                &fixture.document_store,
                PreparedStructuredRevisionSeed {
                    workspace_id,
                    library_id: shard_library_id,
                    document_id,
                    revision_id,
                    source_text: "isolation anchor",
                    chunk_count: 1,
                },
            )
            .await
            .with_context(|| format!("failed to insert prepared {label} artifact"))?;
        }

        // ANN against dim-3 must return library A's chunk and nothing
        // else: library B's vector has a different library and dimension.
        let hits_a = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library_a_id,
                    embedding_model_key: &profile_a_key,
                    query_vector: &[1.0, 0.0, 0.0],
                    n_probe: None,
                },
                16,
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
                VectorSimilaritySearchInput {
                    dim: 4,
                    library_id: library_b_id,
                    embedding_model_key: &profile_b_key,
                    query_vector: &[1.0, 0.0, 0.0, 0.0],
                    n_probe: None,
                },
                16,
                None,
                None,
            )
            .await
            .context("failed ANN search against library B dim-4 shard")?;
        assert_eq!(hits_b.len(), 1, "library B dim-4 shard must return exactly its own chunk");
        assert_eq!(hits_b[0].chunk_id, chunk_b_id);

        assert_primary_chunk_search_overrides_generic_plan_transaction_locally(
            &fixture.temp_database.database_url,
            library_a_id,
            &profile_a_key,
            &[1.0, 0.0, 0.0],
            chunk_a_id,
        )
        .await?;

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
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
        fixture.search_store.ensure_entity_vector_shard(3).await?;
        let physical_only_entity_id = Uuid::now_v7();
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id: physical_only_entity_id,
                workspace_id: workspace.id,
                library_id: library.id,
                primary_label: "Physical-only staged entity".to_string(),
                aliases: Vec::new(),
                entity_type: "concept".to_string(),
                entity_sub_type: None,
                summary: None,
                confidence: Some(1.0),
                support_count: 1,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await?;
        let source_truth_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library.id).await?;

        let inserted = sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             )
             select $1, 3, $2, $3 || $4 || lpad(to_hex(sequence_no), 48, '0'), $5,
                    true, 0, false
             from generate_series(0::bigint, 512::bigint) sequence_no",
        )
        .bind(library.id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(VECTOR_REBUILD_STAGING_PROFILE_PREFIX)
        .bind(format!("{source_truth_version:016x}"))
        .bind("knowledge_chunk_vector_d3")
        .execute(&fixture.postgres)
        .await?;
        assert_eq!(inserted.rows_affected(), 513);
        let physical_only_profile =
            synthetic_staging_profile_key(source_truth_version, Uuid::now_v7());
        sqlx::query(
            "insert into knowledge_entity_vector_d3 (
                 key, vector_id, workspace_id, library_id, entity_id,
                 embedding_model_key, vector_kind, dimensions, embedding,
                 freshness_generation, created_at
             ) values ($1, $2, $3, $4, $5, $6, $7, 3, '[1,0,0]', 1, now())",
        )
        .bind(format!("physical-only-staging-{}", Uuid::now_v7()))
        .bind(Uuid::now_v7())
        .bind(workspace.id)
        .bind(library.id)
        .bind(physical_only_entity_id)
        .bind(&physical_only_profile)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .execute(&fixture.postgres)
        .await?;

        assert_eq!(
            fixture.search_store.discard_abandoned_staged_vector_rebuilds(library.id).await?,
            514
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
        let physical_only_remaining = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&physical_only_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(
            physical_only_remaining, 0,
            "abandoned recovery must discover physical staging rows without manifests"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service with required extensions"]
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
        let active_profile = synthetic_embedding_profile_key(Uuid::now_v7());
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
        .bind(&active_profile)
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
                checksum: crate::content_lifecycle_support::fixture_checksum(
                    "staged-vector-checksum",
                ),
                title: Some("Staged Vector Document".to_string()),
                byte_size: 32,
                normalized_text: Some("staged vector evidence".to_string()),
                text_checksum: Some("staged-vector-text-checksum".to_string()),
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
                occurred_at: None,
                occurred_until: None,
            })
            .await?;
        seed_prepared_structured_revision(
            &fixture.document_store,
            PreparedStructuredRevisionSeed {
                workspace_id: workspace.id,
                library_id: library.id,
                document_id,
                revision_id,
                source_text: "staged vector evidence",
                chunk_count: 1,
            },
        )
        .await?;
        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id: old_entity_id,
                workspace_id: workspace.id,
                library_id: library.id,
                primary_label: "StagedEntity".to_string(),
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

        seed_entity_source_support(&fixture, revision_id, old_entity_id).await?;

        fixture
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: old_chunk_id,
                revision_id,
                embedding_model_key: active_profile.clone(),
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
                embedding_model_key: active_profile.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: 1,
                created_at: now,
            })
            .await?;
        upsert_vector_pair(
            &fixture.search_store,
            VectorPairSeed {
                workspace_id: workspace.id,
                library_id: library.id,
                revision_id,
                chunk_id: same_dimension_orphan_chunk_id,
                entity_id: same_dimension_orphan_entity_id,
                embedding_model_key: &active_profile,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            },
        )
        .await?;

        let staging_source_version =
            repositories::get_library_source_truth_version(&fixture.postgres, library.id).await?;
        let staging_profile = synthetic_staging_profile_key(staging_source_version, Uuid::now_v7());
        let abandoned_profile =
            synthetic_staging_profile_key(staging_source_version, Uuid::now_v7());
        let unprepared_profile =
            synthetic_staging_profile_key(staging_source_version, Uuid::now_v7());

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
        sqlx::query(
            "insert into knowledge_chunk_vector_d3 (
                 key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                 embedding_model_key, vector_kind, dimensions, embedding,
                 freshness_generation, created_at, occurred_at, occurred_until
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, 3, '[0,1,0]', 2, $9, null, null)",
        )
        .bind(format!("missing-staging-manifest-{}", Uuid::now_v7()))
        .bind(Uuid::now_v7())
        .bind(workspace.id)
        .bind(library.id)
        .bind(old_chunk_id)
        .bind(revision_id)
        .bind(&unprepared_profile)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(now)
        .execute(&fixture.postgres)
        .await?;
        fixture
            .search_store
            .discard_staged_vector_rebuild(library.id, 3, &unprepared_profile)
            .await?;
        let missing_manifest_staging_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk_vector_d3
             where library_id = $1 and embedding_model_key = $2",
        )
        .bind(library.id)
        .bind(&unprepared_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(
            missing_manifest_staging_rows, 0,
            "explicit staged cleanup must use the physical shard when its manifest is missing"
        );

        fixture
            .search_store
            .prepare_chunk_vector_rebuild_lane(library.id, 3, &abandoned_profile)
            .await?;
        fixture
            .search_store
            .prepare_entity_vector_rebuild_lane(library.id, 3, &abandoned_profile)
            .await?;
        let abandoned_rows = (0..512)
            .map(|offset| KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                chunk_id: old_chunk_id,
                revision_id,
                embedding_model_key: abandoned_profile.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: 3,
                vector: vec![1.0, 0.0, 0.0],
                freshness_generation: i64::from(offset) + 2,
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
        let authoritative_during_staging = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[1.0, 0.0, 0.0],
                    n_probe: Some(1),
                },
                1,
                None,
                None,
            )
            .await?;
        assert_eq!(
            authoritative_during_staging.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
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
        let authoritative_after_partial_cleanup = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[1.0, 0.0, 0.0],
                    n_probe: None,
                },
                8,
                None,
                None,
            )
            .await?;
        assert_eq!(
            authoritative_after_partial_cleanup.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [old_chunk_id]
        );

        // Recreate source-less rows so failed promotion proves rollback and a
        // successful promotion proves the same source-aware GC invariant.
        upsert_vector_pair(
            &fixture.search_store,
            VectorPairSeed {
                workspace_id: workspace.id,
                library_id: library.id,
                revision_id,
                chunk_id: same_dimension_orphan_chunk_id,
                entity_id: same_dimension_orphan_entity_id,
                embedding_model_key: &active_profile,
                vector: vec![0.0, 0.0, 1.0],
                freshness_generation: 1,
                created_at: now,
            },
        )
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
        let data_locked_active_profile = active_profile.clone();
        let data_locked_staging_profile = staging_profile.clone();
        let mut data_locked_promotion = tokio::spawn(async move {
            data_locked_promotion_store
                .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
                    library_id: data_locked_library_id,
                    dimensions: 3,
                    active_embedding_profile_key: &data_locked_active_profile,
                    staging_embedding_model_key: &data_locked_staging_profile,
                    expected_source_truth_version: source_version.saturating_add(1),
                    chunk_count: Some(1),
                    entity_count: Some(1),
                })
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
        let promotion_active_profile = active_profile.clone();
        let promotion_staging_profile = staging_profile.clone();
        let mut serializer_blocked_promotion = tokio::spawn(async move {
            promotion_store
                .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
                    library_id: promotion_library_id,
                    dimensions: 3,
                    active_embedding_profile_key: &promotion_active_profile,
                    staging_embedding_model_key: &promotion_staging_profile,
                    expected_source_truth_version: source_version.saturating_add(1),
                    chunk_count: Some(1),
                    entity_count: Some(1),
                })
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
            .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
                library_id: library.id,
                dimensions: 3,
                active_embedding_profile_key: &active_profile,
                staging_embedding_model_key: &staging_profile,
                expected_source_truth_version: source_version.saturating_add(1),
                chunk_count: Some(1),
                entity_count: Some(1),
            })
            .await;
        assert!(stale_source_result.is_err());

        // Chunk promotion runs first inside the transaction. An entity count
        // failure therefore proves that the already-applied chunk mutations
        // roll back with the entity lane instead of exposing a split profile.
        let entity_count_failure = fixture
            .search_store
            .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
                library_id: library.id,
                dimensions: 3,
                active_embedding_profile_key: &active_profile,
                staging_embedding_model_key: &staging_profile,
                expected_source_truth_version: source_version,
                chunk_count: Some(1),
                entity_count: Some(2),
            })
            .await;
        assert!(entity_count_failure.is_err());

        let authoritative_chunks = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[1.0, 0.0, 0.0],
                    n_probe: None,
                },
                8,
                None,
                None,
            )
            .await?;
        let authoritative_entities = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[1.0, 0.0, 0.0],
                    n_probe: None,
                },
                8,
            )
            .await?;
        assert_eq!(
            authoritative_chunks.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            [old_chunk_id]
        );
        assert_eq!(
            authoritative_entities.iter().map(|row| row.entity_id).collect::<Vec<_>>(),
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
        .bind(&active_profile)
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
        .bind(&active_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        let same_dimension_entity_orphan_after_rollback = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_entity_vector_d3
             where library_id = $1 and entity_id = $2 and embedding_model_key = $3",
        )
        .bind(library.id)
        .bind(same_dimension_orphan_entity_id)
        .bind(&active_profile)
        .fetch_one(&fixture.postgres)
        .await?;
        assert_eq!(same_dimension_chunk_orphan_after_rollback, 1);
        assert_eq!(same_dimension_entity_orphan_after_rollback, 1);

        fixture
            .search_store
            .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
                library_id: library.id,
                dimensions: 3,
                active_embedding_profile_key: &active_profile,
                staging_embedding_model_key: &staging_profile,
                expected_source_truth_version: source_version,
                chunk_count: Some(1),
                entity_count: Some(1),
            })
            .await?;

        // The promoted entity vector advances to freshness generation 2, so the
        // entity that owns it is re-materialized to the same generation. The
        // entity-vector search only surfaces rows whose source entity shares the
        // vector's `freshness_generation`.
        sqlx::query("update knowledge_entity set freshness_generation = 2 where entity_id = $1")
            .bind(old_entity_id)
            .execute(&fixture.postgres)
            .await?;

        let promoted_chunks = fixture
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[0.0, 1.0, 0.0],
                    n_probe: None,
                },
                8,
                None,
                None,
            )
            .await?;
        let promoted_entities = fixture
            .search_store
            .search_entity_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: 3,
                    library_id: library.id,
                    embedding_model_key: &active_profile,
                    query_vector: &[0.0, 1.0, 0.0],
                    n_probe: None,
                },
                8,
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
        assert_eq!(manifest_profiles, vec![active_profile.clone(), active_profile]);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
