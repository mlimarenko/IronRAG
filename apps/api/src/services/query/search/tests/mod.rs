use super::{
    chunk_rebuild::chunk_rebuild_page_cursor,
    embedding::{
        embedding_usage_token_counts, revision_chunk_embedding_batches,
        take_dimension_learning_batch,
    },
    evidence::merge_technical_fact_hits,
    failures::fail_embed_chunks_if_cancelled,
    persistence::validate_embedding_profile_write,
    rebuild::{
        finish_deferred_manifest_rebuild, validate_rebuilt_chunk_count,
        vector_rebuild_staging_profile_key,
    },
    reuse::{chunk_ids_missing_vectors, chunk_text_reuse_key, embed_coverage_is_complete},
    *,
};
use crate::app::config::Settings;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tokio::time::timeout;

#[test]
fn low_level_vector_write_requires_active_execution_profile_provenance() {
    let library_id = Uuid::from_u128(42);
    assert!(
        validate_embedding_profile_write(
            library_id,
            "embedding-profile:v1:active",
            "embedding-profile:v1:active",
        )
        .is_ok()
    );
    assert!(matches!(
        validate_embedding_profile_write(
            library_id,
            "embedding-profile:v1:stale",
            "embedding-profile:v1:active",
        ),
        Err(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn vector_rebuild_count_must_match_the_active_chunk_inventory() {
    let library_id = Uuid::from_u128(43);

    assert!(validate_rebuilt_chunk_count(library_id, 0, 0).is_ok());
    assert!(validate_rebuilt_chunk_count(library_id, 12_345, 12_345).is_ok());
    let error = validate_rebuilt_chunk_count(library_id, 12_345, 10_000)
        .expect_err("a truncated rebuild must fail closed");
    assert!(matches!(error, QueryServiceError::StateConflict { .. }));
    assert!(error.to_string().contains("rebuild vector-plane --source-library"));
}

#[test]
fn revision_embedding_batches_preserve_order_and_batch_limit() {
    let batches = revision_chunk_embedding_batches(CHUNK_EMBEDDING_BATCH_SIZE * 2 + 1);

    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0], (0..CHUNK_EMBEDDING_BATCH_SIZE).collect::<Vec<_>>());
    assert_eq!(
        batches[1],
        (CHUNK_EMBEDDING_BATCH_SIZE..CHUNK_EMBEDDING_BATCH_SIZE * 2).collect::<Vec<_>>()
    );
    assert_eq!(batches[2], vec![CHUNK_EMBEDDING_BATCH_SIZE * 2]);
    assert!(revision_chunk_embedding_batches(0).is_empty());
}

#[test]
fn embedding_usage_counts_preserve_provider_totals_and_fallback() {
    let explicit = EmbeddingUsageTotals {
        prompt_tokens: 5,
        completion_tokens: 7,
        total_tokens: 20,
        saw_prompt: true,
        saw_completion: true,
        saw_total: true,
    };
    assert_eq!(embedding_usage_token_counts(&explicit), (Some(5), Some(7), Some(20)));

    let derived = EmbeddingUsageTotals { saw_total: false, ..explicit };
    assert_eq!(embedding_usage_token_counts(&derived), (Some(5), Some(7), Some(12)));
    assert_eq!(embedding_usage_token_counts(&EmbeddingUsageTotals::default()), (None, None, None));
}

#[test]
fn unobserved_dimension_consumes_the_first_real_batch_exactly_once() {
    let mut batches = vec![vec![0, 1], vec![2, 3], vec![4]];
    let first = take_dimension_learning_batch(None, &mut batches).unwrap();
    assert_eq!(first, vec![0, 1]);
    assert_eq!(batches, vec![vec![2, 3], vec![4]]);

    let known = EmbeddingDimensions::try_from(3).unwrap();
    let unchanged = batches.clone();
    assert!(take_dimension_learning_batch(Some(known), &mut batches).is_none());
    assert_eq!(batches, unchanged);
}

#[test]
fn cancellation_preserves_committed_attempt_vectors_without_cleanup() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = fail_embed_chunks_if_cancelled(Uuid::from_u128(49), &cancellation)
        .expect_err("cancelled embedding must stop");
    assert!(matches!(error, QueryServiceError::Cancelled));
    assert!(
        !error.preserves_partial_vectors(),
        "cancellation preservation is a lifecycle race invariant, not provider retry policy",
    );
}

fn technical_fact_hit(
    fact_id: Uuid,
    score: f64,
    exact_match: bool,
) -> KnowledgeTechnicalFactSearchRow {
    KnowledgeTechnicalFactSearchRow {
        fact_id,
        document_id: Uuid::from_u128(1),
        workspace_id: Uuid::from_u128(1),
        library_id: Uuid::from_u128(1),
        revision_id: Uuid::from_u128(1),
        fact_kind: "http_method".to_string(),
        normalized_value_text: "GET".to_string(),
        display_value: "GET".to_string(),
        exact_match,
        score,
    }
}

#[test]
fn merge_technical_fact_hits_prefers_the_exact_literal_pass_for_a_shared_fact() {
    // Mirrors the production shape: the raw-query pass only picks the fact
    // up via a weak FTS score, while the exact-literal pass (searching the
    // extracted literal itself, e.g. "GET") lands the 1_000_000 exact-match
    // bonus on the same fact_id. The merge must keep the higher-scoring row.
    let shared_fact_id = Uuid::from_u128(101);
    let raw_only_fact_id = Uuid::from_u128(102);
    let raw_pass = vec![
        technical_fact_hit(shared_fact_id, 0.42, false),
        technical_fact_hit(raw_only_fact_id, 0.10, false),
    ];
    let literal_pass = vec![technical_fact_hit(shared_fact_id, 1_000_000.42, true)];

    let merged = merge_technical_fact_hits([raw_pass, literal_pass], 10);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].fact_id, shared_fact_id);
    assert!(merged[0].exact_match);
    assert_eq!(merged[0].score, 1_000_000.42);
    assert_eq!(merged[1].fact_id, raw_only_fact_id);
}

#[test]
fn merge_technical_fact_hits_deduplicates_identical_hits_across_passes() {
    let fact_id = Uuid::from_u128(201);
    let raw_pass = vec![technical_fact_hit(fact_id, 5.0, false)];
    let literal_pass = vec![technical_fact_hit(fact_id, 5.0, false)];

    let merged = merge_technical_fact_hits([raw_pass, literal_pass], 10);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].fact_id, fact_id);
}

#[test]
fn merge_technical_fact_hits_sorts_the_combined_set_and_truncates_to_the_limit() {
    let passes = (0..5u32)
        .map(|index| {
            vec![technical_fact_hit(Uuid::from_u128(index.into()), f64::from(index), false)]
        })
        .collect::<Vec<_>>();

    let merged = merge_technical_fact_hits(passes, 2);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].score, 4.0);
    assert_eq!(merged[1].score, 3.0);
}

#[tokio::test]
async fn remote_rebuild_serialization_does_not_block_same_library_readers() {
    let service = SearchService::new();
    let library_id = Uuid::from_u128(44);
    let _rebuild = service.vector_rebuild_lock(library_id).lock_owned().await;

    let read_guard =
        timeout(Duration::from_secs(1), service.vector_plane_lock(library_id).read_owned())
            .await
            .expect("the rebuild lock must not exclude data-plane readers");
    drop(read_guard);
}

#[tokio::test]
async fn library_write_lock_does_not_block_another_library() {
    let service = SearchService::new();
    let first_library_id = Uuid::from_u128(45);
    let second_library_id = Uuid::from_u128(46);
    let _first_write = service.vector_plane_lock(first_library_id).write_owned().await;

    let second_read =
        timeout(Duration::from_secs(1), service.vector_plane_lock(second_library_id).read_owned())
            .await
            .expect("one library's promotion must not block another library");
    drop(second_read);
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn rebuild_session_lock_leaves_a_one_connection_app_pool_available() {
    let settings = Settings::from_env().expect("load postgres settings");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&settings.database_url)
        .await
        .expect("connect one-slot postgres pool");
    let library_id = Uuid::now_v7();
    let rebuild_lock = acquire_vector_rebuild_advisory_lock(&settings.database_url, library_id)
        .await
        .expect("acquire dedicated rebuild session lock");

    let value =
        timeout(Duration::from_secs(1), sqlx::query_scalar::<_, i32>("select 1").fetch_one(&pool))
            .await
            .expect("dedicated rebuild session must not consume the app pool")
            .expect("query through one-slot app pool");
    assert_eq!(value, 1);

    drop(rebuild_lock);
    pool.close().await;
}

#[test]
fn staging_profile_keys_are_opaque_unique_and_not_active_aliases() {
    let active_profile = "embedding-profile:v1:active";
    let first = vector_rebuild_staging_profile_key(active_profile, Uuid::from_u128(47), 41);
    let second = vector_rebuild_staging_profile_key(active_profile, Uuid::from_u128(48), 41);

    assert_ne!(first, active_profile);
    assert_ne!(second, active_profile);
    assert_ne!(first, second);
    assert!(first.starts_with(VECTOR_REBUILD_STAGING_PROFILE_PREFIX));
    assert_eq!(first.len(), VECTOR_REBUILD_STAGING_PROFILE_PREFIX.len() + 64);
    assert!(first.starts_with(&format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{:016x}", 41)));
}

#[test]
fn rebuild_keyset_cursor_streams_more_than_ten_thousand_chunks_with_duplicate_indexes() {
    let chunks = (0..10_005usize)
        .map(|ordinal| {
            let mut chunk = make_chunk_for_reuse("neutral fixture");
            chunk.chunk_index = i32::try_from(ordinal / 2).unwrap_or(i32::MAX);
            chunk.chunk_id = Uuid::from_u128((ordinal + 1) as u128);
            chunk
        })
        .collect::<Vec<_>>();
    let mut after = None;
    let mut visited = Vec::new();

    loop {
        let page = chunks
            .iter()
            .filter(|chunk| after.is_none_or(|cursor| (chunk.chunk_index, chunk.chunk_id) > cursor))
            .take(CHUNK_REBUILD_FETCH_PAGE_SIZE)
            .cloned()
            .collect::<Vec<_>>();
        let Some(next_after) = chunk_rebuild_page_cursor(&page) else {
            break;
        };
        visited.extend(page.iter().map(|chunk| chunk.chunk_id));
        if page.len() < CHUNK_REBUILD_FETCH_PAGE_SIZE {
            break;
        }
        after = Some(next_after);
    }

    assert_eq!(visited.len(), chunks.len());
    assert_eq!(visited.iter().copied().collect::<BTreeSet<_>>().len(), chunks.len());
}

#[test]
fn deferred_rebuild_returns_primary_error_after_successful_reconciliation() {
    let result = finish_deferred_manifest_rebuild::<()>(
        Err(QueryServiceError::StateConflict { message: "primary".to_string() }),
        Ok(()),
    );

    assert!(matches!(result, Err(QueryServiceError::StateConflict { .. })));
}

#[test]
fn deferred_rebuild_fails_closed_when_reconciliation_also_fails() {
    let result = finish_deferred_manifest_rebuild::<()>(
        Err(QueryServiceError::StateConflict { message: "primary".to_string() }),
        Err(QueryServiceError::StateConflict { message: "reconcile".to_string() }),
    );

    assert!(matches!(result, Err(QueryServiceError::Internal(_))));
}

#[test]
fn chunk_text_reuse_key_is_content_addressed() {
    let left = make_chunk_for_reuse("alpha\nbeta");
    let right = make_chunk_for_reuse("alpha\nbeta");
    let changed = make_chunk_for_reuse("alpha\ngamma");
    let blank = make_chunk_for_reuse("   ");

    assert_eq!(chunk_text_reuse_key(&left), chunk_text_reuse_key(&right));
    assert_ne!(chunk_text_reuse_key(&left), chunk_text_reuse_key(&changed));
    assert_eq!(chunk_text_reuse_key(&blank), None);
}

fn make_chunk_for_reuse(normalized_text: &str) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("text".to_string()),
        content_text: normalized_text.to_string(),
        normalized_text: normalized_text.to_string(),
        span_start: Some(0),
        span_end: Some(i32::try_from(normalized_text.len()).unwrap_or(i32::MAX)),
        token_count: None,
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
    }
}

// BUG B (c): after a transient failure that preserved the already-persisted
// batches, the retry embeds ONLY the missing remainder — chunks whose
// vectors survived are skipped.
#[test]
fn retry_embeds_only_chunks_missing_vectors() {
    let chunk_a = Uuid::now_v7();
    let chunk_b = Uuid::now_v7();
    let chunk_c = Uuid::now_v7();
    let all = [chunk_a, chunk_b, chunk_c];

    // Fresh revision: nothing persisted yet, so every chunk is embedded.
    let none_covered = BTreeSet::new();
    assert_eq!(chunk_ids_missing_vectors(&all, &none_covered), vec![chunk_a, chunk_b, chunk_c]);

    // Retry after a blip persisted A and B: only C remains.
    let mut partially_covered = BTreeSet::new();
    partially_covered.insert(chunk_a);
    partially_covered.insert(chunk_b);
    assert_eq!(chunk_ids_missing_vectors(&all, &partially_covered), vec![chunk_c]);

    // Final retry after the remainder also persisted: nothing left to embed.
    let fully_covered: BTreeSet<Uuid> = all.iter().copied().collect();
    assert!(chunk_ids_missing_vectors(&all, &fully_covered).is_empty());
}

// BUG B (d): readiness stays gated on FULL coverage. A revision is only
// complete when embedded + reused == total; a half-embedded revision
// (transient failure preserved partial vectors) never passes the gate.
#[test]
fn embed_readiness_is_gated_on_full_coverage() {
    // Fully covered by a single clean run.
    assert!(embed_coverage_is_complete(3, 0, 3));
    // Fully covered by resume: 1 reused from a prior attempt + 2 embedded now.
    assert!(embed_coverage_is_complete(2, 1, 3));
    // Half-embedded: a transient failure left only 2 of 3 covered.
    assert!(!embed_coverage_is_complete(2, 0, 3));
    // Still short after a partial resume.
    assert!(!embed_coverage_is_complete(1, 1, 3));
}
