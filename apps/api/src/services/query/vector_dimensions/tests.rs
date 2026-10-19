use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::inventory::{
    resolve_profile_inventory_once_for_test, validate_embedding_profile_inventory,
    validate_embedding_profile_inventory_version, validate_index_state_against_generation,
};
use super::*;

fn dimensions(value: u64) -> EmbeddingDimensions {
    EmbeddingDimensions::try_from(value).unwrap()
}

#[test]
fn validates_expected_embedding_dimensions() {
    assert_eq!(
        i32::try_from(3usize).unwrap(),
        validate_embedding_vector_dimensions(3, &[0.0, 1.0, 2.0], "test vector").unwrap()
    );
}

#[test]
fn rejects_unexpected_embedding_dimensions() {
    let error = validate_embedding_vector_dimensions(3, &[0.0, 1.0], "test vector")
        .unwrap_err()
        .to_string();
    assert!(error.contains("expected 3 dimensions"));
    assert!(error.contains("got 2"));
}

#[test]
fn dimension_resolution_uses_only_declared_or_exact_manifest_metadata() {
    assert_eq!(
        select_declared_or_manifest_dimension(Some(dimensions(768)), None).unwrap(),
        EmbeddingDimensionState::Known(dimensions(768)),
    );
    assert_eq!(
        select_declared_or_manifest_dimension(None, Some(1536)).unwrap(),
        EmbeddingDimensionState::Known(dimensions(1536)),
    );
    assert_eq!(
        select_declared_or_manifest_dimension(None, None).unwrap(),
        EmbeddingDimensionState::Unobserved,
    );
}

#[test]
fn unobserved_dimension_fails_closed_outside_chunk_creation() {
    let error =
        require_observed_dimensions(Uuid::from_u128(40), EmbeddingDimensionState::Unobserved)
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn rejects_unsupported_manifest_dimension_claims() {
    assert!(select_declared_or_manifest_dimension(None, Some(0)).is_err());
    assert!(select_declared_or_manifest_dimension(None, Some(4_001)).is_err());
}

#[test]
fn embedding_batch_requires_exact_count_uniform_width_and_reported_width() {
    let vectors = vec![vec![0.0, 1.0], vec![2.0, 3.0]];
    assert_eq!(
        validate_embedding_batch_dimensions(2, 2, &vectors, None, "test batch").unwrap(),
        dimensions(2),
    );
    assert!(validate_embedding_batch_dimensions(1, 2, &vectors, None, "test batch").is_err());
    assert!(
        validate_embedding_batch_dimensions(
            2,
            2,
            &[vec![0.0, 1.0], vec![2.0]],
            None,
            "test batch",
        )
        .is_err()
    );
    assert!(validate_embedding_batch_dimensions(2, 3, &vectors, None, "test batch").is_err());
}

#[test]
fn embedding_batch_rejects_empty_non_finite_and_out_of_range_vectors() {
    assert!(validate_embedding_batch_dimensions(0, 0, &[], None, "test batch").is_err());
    assert!(
        validate_embedding_batch_dimensions(1, 2, &[vec![0.0, f32::NAN]], None, "test batch",)
            .is_err()
    );
    assert!(
        validate_embedding_batch_dimensions(1, 4_001, &[vec![0.0; 4_001]], None, "test batch",)
            .is_err()
    );
}

#[test]
fn active_embedding_profile_requires_exact_persisted_vector_inventory() {
    let library_id = Uuid::from_u128(41);
    assert_eq!(
        validate_embedding_profile_inventory(library_id, &[1536], 2, 2).unwrap(),
        EmbeddingProfileIndexState::Ready { dimensions: 1536 }
    );

    let error = validate_embedding_profile_inventory(library_id, &[], 2, 0).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
    let message = error.to_string();
    assert!(message.contains("rebuild vector-plane"));
    assert!(message.contains("--source-library"));
    assert!(message.contains(&library_id.to_string()));

    let ambiguous =
        validate_embedding_profile_inventory(library_id, &[768, 1536], 2, 2).unwrap_err();
    assert!(matches!(
        ambiguous.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));

    let partial = validate_embedding_profile_inventory(library_id, &[1536], 2, 1).unwrap_err();
    assert!(partial.to_string().contains("incomplete vector inventory"));
}

#[test]
fn empty_library_has_an_explicit_non_vector_search_state() {
    let library_id = Uuid::from_u128(42);

    assert_eq!(
        validate_embedding_profile_inventory(library_id, &[], 0, 0).unwrap(),
        EmbeddingProfileIndexState::Empty
    );
}

#[test]
fn empty_library_rejects_stale_vectors() {
    let library_id = Uuid::from_u128(44);

    let error = validate_embedding_profile_inventory(library_id, &[1536], 0, 1).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn non_empty_inventory_without_active_generation_fails_closed() {
    let library_id = Uuid::from_u128(45);
    let version = EmbeddingProfileInventoryVersion {
        active_vector_generation: 0,
        source_truth_version: 1,
        has_ready_vector: false,
    };

    let error = validate_index_state_against_generation(
        library_id,
        version,
        EmbeddingProfileIndexState::Ready { dimensions: 1536 },
    )
    .unwrap_err();

    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn non_empty_library_without_generation_cannot_silently_downgrade_to_empty() {
    let library_id = Uuid::from_u128(49);

    let error = validate_embedding_profile_inventory(library_id, &[], 2, 0).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn empty_inventory_is_healthy_without_active_generation() {
    let version = EmbeddingProfileInventoryVersion {
        active_vector_generation: 0,
        source_truth_version: 1,
        has_ready_vector: false,
    };

    assert_eq!(
        validate_index_state_against_generation(
            Uuid::from_u128(46),
            version,
            EmbeddingProfileIndexState::Empty,
        )
        .unwrap(),
        EmbeddingProfileIndexState::Empty
    );
}

#[test]
fn inventory_version_fence_rejects_generation_or_source_change() {
    let library_id = Uuid::from_u128(47);
    let expected = EmbeddingProfileInventoryVersion {
        active_vector_generation: 7,
        source_truth_version: 11,
        has_ready_vector: true,
    };

    assert!(validate_embedding_profile_inventory_version(library_id, expected, expected).is_ok());
    assert!(
        validate_embedding_profile_inventory_version(
            library_id,
            expected,
            EmbeddingProfileInventoryVersion { active_vector_generation: 8, ..expected },
        )
        .is_err()
    );
    assert!(
        validate_embedding_profile_inventory_version(
            library_id,
            expected,
            EmbeddingProfileInventoryVersion { source_truth_version: 12, ..expected },
        )
        .is_err()
    );
}

#[tokio::test]
async fn profile_inventory_cache_is_generation_fenced_and_explicitly_invalidated() {
    let library_id = Uuid::from_u128(48);
    invalidate_library_embedding_profile_inventory(library_id);
    let calls = Arc::new(AtomicUsize::new(0));
    let version = EmbeddingProfileInventoryVersion {
        active_vector_generation: 3,
        source_truth_version: 5,
        has_ready_vector: true,
    };

    for _ in 0..2 {
        let calls = Arc::clone(&calls);
        assert_eq!(
            resolve_profile_inventory_once_for_test(
                library_id,
                version,
                "profile-cache-fence",
                move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
                },
            )
            .await
            .unwrap(),
            EmbeddingProfileIndexState::Ready { dimensions: 3 }
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let calls_after_generation = Arc::clone(&calls);
    resolve_profile_inventory_once_for_test(
        library_id,
        EmbeddingProfileInventoryVersion { active_vector_generation: 4, ..version },
        "profile-cache-fence",
        move || async move {
            calls_after_generation.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    invalidate_library_embedding_profile_inventory(library_id);
    let calls_after_invalidation = Arc::clone(&calls);
    resolve_profile_inventory_once_for_test(
        library_id,
        version,
        "profile-cache-fence",
        move || async move {
            calls_after_invalidation.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn profile_inventory_cache_singleflights_concurrent_misses() {
    let library_id = Uuid::from_u128(50);
    invalidate_library_embedding_profile_inventory(library_id);
    let calls = Arc::new(AtomicUsize::new(0));
    let version = EmbeddingProfileInventoryVersion {
        active_vector_generation: 9,
        source_truth_version: 13,
        has_ready_vector: true,
    };
    let futures = (0..16).map(|_| {
        let calls = Arc::clone(&calls);
        async move {
            resolve_profile_inventory_once_for_test(
                library_id,
                version,
                "profile-concurrent-fence",
                move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
                },
            )
            .await
        }
    });

    let results = futures::future::join_all(futures).await;

    assert!(results.into_iter().all(|result| result.is_ok()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn active_embedding_profile_must_match_the_query_vector_provenance() {
    let library_id = Uuid::from_u128(43);
    assert!(
        validate_active_embedding_profile_key(
            library_id,
            "embedding-profile:v1:active",
            "embedding-profile:v1:active",
        )
        .is_ok()
    );
    let error = validate_active_embedding_profile_key(
        library_id,
        "embedding-profile:v1:query",
        "embedding-profile:v1:active",
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::StateConflict { .. })
    ));
}

#[test]
fn negative_cache_preserves_typed_binding_failure() {
    let cached = cached_dimension_resolution(Err(anyhow::Error::new(
        QueryServiceError::BindingNotConfigured { message: "query_provider_failed".to_string() },
    )));
    let error = cached_dimension_state(&cached).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::BindingNotConfigured { .. })
    ));
}

#[test]
fn negative_cache_does_not_infer_binding_failure_from_message() {
    let cached = cached_dimension_resolution(Err(anyhow!("query_binding_not_configured")));
    let error = cached_dimension_state(&cached).unwrap_err();

    assert!(error.downcast_ref::<QueryServiceError>().is_none());
}

#[test]
fn negative_cache_preserves_typed_provider_failure() {
    let cached = cached_dimension_resolution(Err(anyhow::Error::new(QueryServiceError::Provider(
        ProviderFailure::unavailable("query_binding_not_configured"),
    ))));
    let error = cached_dimension_state(&cached).unwrap_err();

    assert!(matches!(
        error.downcast_ref::<QueryServiceError>(),
        Some(QueryServiceError::Provider(_))
    ));
}

#[tokio::test]
async fn concurrent_cold_dimension_resolution_is_singleflight() {
    let library_id = Uuid::now_v7();
    invalidate_library_vector_index_dimensions(library_id);
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();

    for _ in 0..8 {
        let calls = Arc::clone(&calls);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            resolve_dimension_once_for_test(
                library_id,
                "embedding-profile:v1:shared",
                || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(1536)
                },
            )
            .await
        }));
    }

    for task in tasks {
        assert_eq!(task.await.expect("dimension task should join").unwrap(), 1536);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    invalidate_library_vector_index_dimensions(library_id);
}

#[tokio::test]
async fn invalidation_transitions_cached_unobserved_state_to_a_learned_claim() {
    let library_id = Uuid::now_v7();
    let profile = "embedding-profile:v1:learned-transition";
    invalidate_library_vector_index_dimensions(library_id);
    let cell = dimension_cell(library_id, profile);
    let cached = cell
        .get_or_init(|| async {
            cached_dimension_resolution(Ok(EmbeddingDimensionState::Unobserved))
        })
        .await;
    assert_eq!(cached_dimension_state(cached).unwrap(), EmbeddingDimensionState::Unobserved);

    invalidate_library_vector_index_dimensions(library_id);
    assert_eq!(
        resolve_dimension_once_for_test(library_id, profile, || async { Ok(3) }).await.unwrap(),
        3
    );
    invalidate_library_vector_index_dimensions(library_id);
}

#[tokio::test]
async fn failed_dimension_resolution_is_coalesced_and_retryable_after_invalidation() {
    let library_id = Uuid::now_v7();
    invalidate_library_vector_index_dimensions(library_id);
    let first =
        resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
            Err(anyhow!("synthetic transient failure"))
        })
        .await;
    assert!(first.is_err());

    let repeated =
        resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
            Ok(768)
        })
        .await;
    assert!(repeated.is_err(), "the short negative-cache window must coalesce retries");

    invalidate_library_vector_index_dimensions(library_id);
    let recovered =
        resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
            Ok(768)
        })
        .await
        .unwrap();
    assert_eq!(recovered, 768);
    invalidate_library_vector_index_dimensions(library_id);
}

#[tokio::test]
async fn dimension_cache_isolated_by_embedding_execution_profile() {
    let library_id = Uuid::now_v7();
    invalidate_library_vector_index_dimensions(library_id);

    let first =
        resolve_dimension_once_for_test(library_id, "embedding-profile:v1:first", || async {
            Ok(1536)
        })
        .await
        .unwrap();
    let second =
        resolve_dimension_once_for_test(library_id, "embedding-profile:v1:second", || async {
            Ok(768)
        })
        .await
        .unwrap();

    assert_eq!(first, 1536);
    assert_eq!(second, 768);
    invalidate_library_vector_index_dimensions(library_id);
}
