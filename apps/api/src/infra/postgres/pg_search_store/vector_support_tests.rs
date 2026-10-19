use super::*;

mod database_url {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/support/database_url.rs"));
}
mod temp_postgres_database {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/support/temp_postgres_database.rs"));
}

#[tokio::test]
#[ignore = "requires local postgres service with pgvector"]
async fn filtered_hnsw_configuration_is_bounded_and_transaction_local() -> anyhow::Result<()> {
    let settings = crate::app::config::Settings::from_env()?;
    let database = temp_postgres_database::TempPostgresDatabase::create(
        &settings.database_url,
        "vector_settings",
    )
    .await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database.database_url)
        .await?;
    let result = async {
        sqlx::query("create extension vector").execute(&pool).await?;
        // Loading the extension registers its GUCs on this session.
        sqlx::query("select '[1,0,0]'::vector").execute(&pool).await?;
        let settings_sql =
            "select current_setting('hnsw.ef_search'), current_setting('hnsw.iterative_scan'),
        current_setting('hnsw.max_scan_tuples'), current_setting('hnsw.scan_mem_multiplier')";
        let before: (String, String, String, String) =
            sqlx::query_as(settings_sql).fetch_one(&pool).await?;
        let mut tx = pool.begin().await?;
        configure_hnsw_search(
            &mut tx,
            pg_hnsw_search_params_from_overrides(
                Some(u64::MAX),
                None,
                Some(u64::MAX),
                Some(u64::MAX),
            ),
        )
        .await?;
        let inside: (String, String, String, String) =
            sqlx::query_as(settings_sql).fetch_one(&mut *tx).await?;
        assert_eq!(
            inside,
            (
                PG_HNSW_MAX_EF_SEARCH.to_string(),
                "strict_order".to_string(),
                PG_HNSW_MAX_SCAN_TUPLES.to_string(),
                PG_HNSW_MAX_SCAN_MEM_MULTIPLIER.to_string()
            )
        );
        tx.commit().await?;
        let after: (String, String, String, String) =
            sqlx::query_as(settings_sql).fetch_one(&pool).await?;
        assert_eq!(after, before);
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

/// Fragments the ANN chunk-similarity SQL must carry to join active chunks to
/// their readable, prepared revisions before the top-k bound.
const CHUNK_VECTOR_SIMILARITY_FRAGMENTS: &[&str] = &[
    "join knowledge_chunk c",
    "c.document_id",
    "c.workspace_id = v.workspace_id",
    "join knowledge_document d",
    "d.workspace_id = c.workspace_id",
    "current_revision.revision_id = v.revision_id",
    "d.document_state = 'active'",
    "d.deleted_at is null",
    "join knowledge_structured_revision sr",
    "sr.revision_id = v.revision_id",
    "sr.document_id = c.document_id",
    "sr.workspace_id = v.workspace_id",
    "sr.library_id = v.library_id",
    "sr.preparation_state = 'prepared'",
    "and c.chunk_state = 'ready'",
    "order by v.embedding <=> $3::vector(3)",
];

fn assert_chunk_vector_joins_active_chunks(sql: &str) {
    for fragment in CHUNK_VECTOR_SIMILARITY_FRAGMENTS {
        assert!(sql.contains(*fragment), "chunk vector SQL is missing {fragment:?}");
    }
    assert!(
        sql.find("sr.preparation_state = 'prepared'").unwrap() < sql.rfind("limit $7").unwrap(),
        "preparing revisions must be excluded before the ANN top-k bound",
    );
    assert!(!sql.contains("preparation_state = 'preparing'"));
}

#[test]
fn chunk_vector_sql_joins_active_chunks_before_top_k() {
    let sql = chunk_vector_similarity_sql("\"knowledge_chunk_vector_3\"", "vector(3)");
    assert_chunk_vector_joins_active_chunks(&sql);
}

/// Guards the decision that the primary chunk lane keeps `order by embedding <=> $3`
/// on the base scan so pgvector can select the HNSW index. A materialized lane
/// (`with lane as materialized` + `candidate_chunks`) is the rejected shape: it
/// computes and sorts the whole lane and makes `hnsw.ef_search` /
/// `hnsw.iterative_scan` unreachable for the chunk lane. See
/// docs/en/CAPACITY-PLANNING.md, "Filtered HNSW query bounds".
#[test]
fn chunk_vector_sql_keeps_the_ann_order_on_the_base_scan() {
    let sql = chunk_vector_similarity_sql("\"knowledge_chunk_vector_d3\"", "vector(3)");

    assert!(
        sql.contains("order by v.embedding <=> $3::vector(3), v.chunk_id asc"),
        "the primary chunk query must order the base scan by the ANN distance, \
         not by a precomputed materialized-lane column",
    );
    assert!(
        !sql.contains("with lane as materialized") && !sql.contains("candidate_chunks"),
        "a materialized lane structurally forecloses pgvector HNSW and is not the chosen shape",
    );
    assert!(
        sql.find("join knowledge_chunk c").unwrap() < sql.rfind("limit $7").unwrap(),
        "the active-chunk join must stay on the base scan before the bounded ANN top-k",
    );

    let exact = chunk_vector_exact_similarity_sql("\"knowledge_chunk_vector_d3\"", "vector(3)");
    assert!(
        exact.contains("order by (v.embedding <=> $3::vector(3)) + 0.0, v.chunk_id asc"),
        "the exact retry keeps its distinct `+ 0.0` order that disables ANN selection",
    );
    assert_ne!(exact, sql, "the exact retry is a distinct bounded shape, not a second copy");
}

#[test]
fn document_vector_rescue_selects_one_exact_best_chunk_per_unseen_document() {
    let sql = chunk_vector_document_rescue_sql("\"knowledge_chunk_vector_d3\"", "vector(3)");

    assert!(sql.contains("with eligible as materialized"));
    assert!(sql.contains("(v.embedding <=> $3::vector(3)) + 0.0"));
    assert!(sql.contains("not (c.document_id = any($7::uuid[]))"));
    assert!(sql.contains("partition by document_id"));
    assert!(sql.contains("order by distance, chunk_id"));
    assert!(sql.contains("where document_rank = 1"));
    assert!(sql.contains("order by distance, document_id, chunk_id"));
    assert!(sql.contains("limit $8"));
    assert!(sql.contains("current_revision.revision_id = v.revision_id"));
    assert!(sql.contains("sr.preparation_state = 'prepared'"));
    assert!(sql.contains("c.chunk_state = 'ready'"));
}

#[test]
fn entity_vector_sql_filters_inactive_entities_before_top_k() {
    let sql = entity_vector_similarity_sql("\"knowledge_entity_vector_d3\"", "vector(3)");

    assert!(sql.contains("from \"knowledge_entity_vector_d3\" v"));
    assert!(sql.contains("join knowledge_entity e"));
    assert!(sql.contains("e.entity_id = v.entity_id"));
    assert!(sql.contains("e.library_id = v.library_id"));
    assert!(sql.contains("e.entity_state = 'active'"));
    assert!(
        sql.find("e.entity_state = 'active'").unwrap() < sql.rfind("limit $5").unwrap(),
        "inactive entity vectors must not consume the bounded ANN result set",
    );
}

#[test]
fn filtered_hnsw_search_parameters_are_explicit_and_bounded() {
    let defaults = pg_hnsw_search_params_from_overrides(None, None, None, None);
    assert_eq!(defaults.ef_search, PG_HNSW_DEFAULT_EF_SEARCH);
    assert_eq!(defaults.max_scan_tuples, PG_HNSW_DEFAULT_MAX_SCAN_TUPLES);
    assert_eq!(defaults.scan_mem_multiplier, PG_HNSW_DEFAULT_SCAN_MEM_MULTIPLIER);

    let bounded = pg_hnsw_search_params_from_overrides(
        Some(u64::MAX),
        Some(7),
        Some(u64::MAX),
        Some(u64::MAX),
    );
    assert_eq!(bounded.ef_search, PG_HNSW_MAX_EF_SEARCH);
    assert_eq!(bounded.max_scan_tuples, PG_HNSW_MAX_SCAN_TUPLES);
    assert_eq!(bounded.scan_mem_multiplier, PG_HNSW_MAX_SCAN_MEM_MULTIPLIER);

    let minimums = pg_hnsw_search_params_from_overrides(None, Some(0), Some(0), Some(0));
    assert_eq!(minimums.ef_search, 1);
    assert_eq!(minimums.max_scan_tuples, 1);
    assert_eq!(minimums.scan_mem_multiplier, 1);

    let configured = pg_hnsw_search_params_from_overrides(None, Some(123), Some(45_000), Some(3));
    assert_eq!(configured.ef_search, 123);
    assert_eq!(configured.max_scan_tuples, 45_000);
    assert_eq!(configured.scan_mem_multiplier, 3);

    let request_override =
        pg_hnsw_search_params_from_overrides(Some(321), Some(123), Some(45_000), Some(3));
    assert_eq!(request_override.ef_search, 321);
}

#[test]
fn exact_fallback_threshold_is_explicit_disableable_and_bounded() {
    assert_eq!(
        pg_hnsw_exact_fallback_max_rows_from_override(None),
        PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS
    );
    assert_eq!(pg_hnsw_exact_fallback_max_rows_from_override(Some(0)), 0);
    assert_eq!(
        pg_hnsw_exact_fallback_max_rows_from_override(Some(u64::MAX)),
        PG_HNSW_MAX_EXACT_FALLBACK_ROWS
    );
}

#[test]
fn ann_underfill_diagnostics_exclude_naturally_small_lanes() {
    for (found, requested, manifest, underfilled) in [
        (3, 8, 3, false),
        (3, 8, 7, false),
        (0, 8, 0, false),
        (3, 8, 8, true),
        (8, 8, 8, false),
        (0, 0, 1, true),
    ] {
        assert_eq!(
            ann_search_underfilled(found, requested, manifest),
            underfilled,
            "found={found}, requested={requested}, manifest={manifest}",
        );
    }
}

#[test]
fn exact_retry_is_limited_to_underfilled_small_manifest_lanes() {
    assert!(should_retry_exact_ann(3, 8, 1_000, 10_000));
    assert!(!should_retry_exact_ann(8, 8, 1_000, 10_000));
    assert!(!should_retry_exact_ann(3, 8, 7, 10_000));
    assert!(!should_retry_exact_ann(3, 8, 10_001, 10_000));
    assert!(!should_retry_exact_ann(3, 8, 1_000, 0));
}

#[test]
fn exact_document_rescue_obeys_the_same_hard_row_bound() {
    assert!(should_run_exact_document_rescue(1, 2, 10_000, 10_000));
    assert!(!should_run_exact_document_rescue(1, 2, 10_001, 10_000));
    assert!(!should_run_exact_document_rescue(1, 2, 10_000, 0));
    assert!(!should_run_exact_document_rescue(2, 2, 10_000, 10_000));
    assert!(!should_run_exact_document_rescue(1, 2, 1, 10_000));
}

#[test]
fn exact_retry_sql_disables_hnsw_ordering_without_removing_lane_filters() {
    let chunk_sql = chunk_vector_exact_similarity_sql("\"knowledge_chunk_vector_d3\"", "vector(3)");
    assert!(chunk_sql.contains("v.embedding_model_key = $2"));
    assert!(chunk_sql.contains("v.vector_kind = $6"));
    assert!(chunk_sql.contains("order by (v.embedding <=> $3::vector(3)) + 0.0"));

    let entity_sql =
        entity_vector_exact_similarity_sql("\"knowledge_entity_vector_d3\"", "vector(3)");
    assert!(entity_sql.contains("v.embedding_model_key = $2"));
    assert!(entity_sql.contains("v.vector_kind = $4"));
    assert!(entity_sql.contains("order by (v.embedding <=> $3::vector(3)) + 0.0"));
}

#[test]
fn chunk_vector_coverage_sql_counts_only_active_chunks() {
    let sql = active_chunk_vector_count_sql("\"knowledge_chunk_vector_d3\"");

    assert!(sql.contains("from \"knowledge_chunk_vector_d3\" v"));
    assert!(sql.contains("join knowledge_chunk c"));
    assert!(sql.contains("c.chunk_id = v.chunk_id"));
    assert!(sql.contains("c.revision_id = v.revision_id"));
    assert!(sql.contains("c.workspace_id = v.workspace_id"));
    assert!(sql.contains("c.library_id = v.library_id"));
    assert!(sql.contains("and c.chunk_state = 'ready'"));
    assert!(
        !sql.contains("knowledge_structured_revision"),
        "publication coverage must remain observable while the exact revision is preparing",
    );
}

/// Fragments present once in the dimension-count set query, or forbidden
/// because they would reintroduce a per-shard projection scan.
const DIMENSION_COUNTS_REQUIRED_FRAGMENTS: &[&str] = &[
    "from \"knowledge_chunk_vector_d3\" v",
    "from \"knowledge_chunk_vector_d7\" v",
    "from knowledge_chunk c",
    "v.chunk_id = c.chunk_id",
    "v.revision_id = c.revision_id",
    "v.library_id = c.library_id",
];

const DIMENSION_COUNTS_FORBIDDEN_FRAGMENTS: &[&str] =
    &["count(distinct v.chunk_id)", "preparation_state = 'preparing'"];

/// Fragments each per-dimension arm must repeat exactly twice — one arm per
/// shard — and the one set-level `union all` that joins them.
const DIMENSION_COUNTS_OCCURRENCES: &[(&str, usize)] = &[
    ("union all", 1),
    ("count(*)::bigint as active_count", 2),
    ("and exists (", 4),
    ("from content_current_revision current_revision", 2),
    ("and c.chunk_state = 'ready'", 2),
    ("join knowledge_document d", 2),
    ("d.workspace_id = c.workspace_id", 2),
    ("d.library_id = c.library_id", 2),
    ("current_revision.document_id = d.document_id", 2),
    ("current_revision.revision_id = c.revision_id", 2),
    ("d.document_state = 'active'", 2),
    ("d.deleted_at is null", 2),
    ("join knowledge_structured_revision sr", 2),
    ("sr.revision_id = c.revision_id", 2),
    ("sr.document_id = c.document_id", 2),
    ("sr.workspace_id = c.workspace_id", 2),
    ("sr.library_id = c.library_id", 2),
    ("sr.preparation_state = 'prepared'", 2),
    ("v.workspace_id = c.workspace_id", 2),
];

fn assert_dimension_counts_sql_shape(sql: &str) {
    for fragment in DIMENSION_COUNTS_REQUIRED_FRAGMENTS {
        assert!(sql.contains(*fragment), "dimension-counts SQL is missing {fragment:?}");
    }
    for (fragment, expected) in DIMENSION_COUNTS_OCCURRENCES {
        assert_eq!(
            sql.matches(*fragment).count(),
            *expected,
            "dimension-counts occurrence of {fragment:?}",
        );
    }
    for forbidden in DIMENSION_COUNTS_FORBIDDEN_FRAGMENTS {
        assert!(!sql.contains(*forbidden), "dimension-counts SQL must not contain {forbidden:?}");
    }
}

#[test]
fn chunk_vector_dimension_sql_uses_one_set_query_and_ignores_legacy_only_shards() {
    let sql = active_chunk_vector_dimension_counts_sql(&[
        (3, "knowledge_chunk_vector_d3".to_string()),
        (7, "knowledge_chunk_vector_d7".to_string()),
    ])
    .unwrap()
    .unwrap();

    assert_dimension_counts_sql_shape(&sql);
    assert!(active_chunk_vector_dimension_counts_sql(&[]).unwrap().is_none());
}

#[test]
fn chunk_vector_dimension_sql_rejects_manifest_dimension_relation_mismatch() {
    let result = active_chunk_vector_dimension_counts_sql(&[(
        1536,
        "knowledge_chunk_vector_d768".to_string(),
    )]);

    assert!(result.is_err());
}

#[test]
fn active_chunk_vector_dimensions_drop_empty_and_rank_by_live_rows() {
    let dimensions =
        rank_active_chunk_vector_dimensions(vec![(384, 0), (768, 2), (1536, 5), (1024, 5)])
            .unwrap();

    assert_eq!(dimensions, vec![1536, 1024, 768]);
}

#[test]
fn chunk_vector_profile_inventory_returns_ranked_dimensions_and_exact_count() {
    let inventory = chunk_vector_profile_inventory(vec![(384, 0), (768, 2), (1536, 5)]).unwrap();

    assert_eq!(inventory.dimensions, vec![1536, 768]);
    assert_eq!(inventory.active_vector_count, 7);
    assert!(chunk_vector_profile_inventory(vec![(768, -1)]).is_err());
}

#[test]
fn empty_profile_inventory_has_no_dimension_or_active_vectors() {
    let inventory = chunk_vector_profile_inventory(vec![(384, 0), (768, 0)]).unwrap();

    assert!(inventory.dimensions.is_empty());
    assert_eq!(inventory.active_vector_count, 0);
}

#[test]
fn deferred_chunk_relation_requires_one_valid_prepared_lane() {
    let library_id = Uuid::now_v7();
    let mut first = deferred_chunk_row(library_id, 3, "profile-a");
    let second = deferred_chunk_row(library_id, 3, "profile-a");

    assert_eq!(
        deferred_chunk_vector_relation(&[first.clone(), second]).unwrap(),
        Some("knowledge_chunk_vector_d3".to_string())
    );
    first.vector.pop();
    assert!(deferred_chunk_vector_relation(&[first]).is_err());
    assert_eq!(deferred_chunk_vector_relation(&[]).unwrap(), None);
}

#[test]
fn deferred_entity_relation_rejects_cross_library_batches() {
    let first = deferred_entity_row(Uuid::now_v7(), 3, "profile-a");
    let second = deferred_entity_row(Uuid::now_v7(), 3, "profile-a");

    assert!(deferred_entity_vector_relation(&[first, second]).is_err());
}

fn deferred_chunk_row(
    library_id: Uuid,
    dimensions: i32,
    embedding_model_key: &str,
) -> KnowledgeChunkVectorRow {
    KnowledgeChunkVectorRow {
        vector_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id,
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        embedding_model_key: embedding_model_key.to_string(),
        vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
        dimensions,
        vector: vec![0.0; usize::try_from(dimensions).unwrap()],
        freshness_generation: 1,
        created_at: Utc::now(),
        occurred_at: None,
        occurred_until: None,
    }
}

fn deferred_entity_row(
    library_id: Uuid,
    dimensions: i32,
    embedding_model_key: &str,
) -> KnowledgeEntityVectorRow {
    KnowledgeEntityVectorRow {
        vector_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id,
        entity_id: Uuid::now_v7(),
        embedding_model_key: embedding_model_key.to_string(),
        vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
        dimensions,
        vector: vec![0.0; usize::try_from(dimensions).unwrap()],
        freshness_generation: 1,
        created_at: Utc::now(),
    }
}
