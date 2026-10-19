use super::*;

fn vector_write_fence(
    expected_source_truth_version: i64,
    ingest_attempt: Option<IngestVectorWriteFence>,
    advance_source_truth_version: bool,
) -> VectorWriteFence {
    VectorWriteFence {
        expected_source_truth_version,
        embedding_profile_key: format!(
            "{EMBEDDING_PROFILE_PREFIX}{}",
            "0123456789abcdef".repeat(4)
        ),
        ingest_attempt,
        advance_source_truth_version,
    }
}

#[test]
fn attempt_fenced_vector_write_accepts_unrelated_library_source_advance() {
    let fence = vector_write_fence(
        41,
        Some(IngestVectorWriteFence { attempt_id: Uuid::now_v7(), revision_id: Uuid::now_v7() }),
        false,
    );

    assert!(validate_vector_source_truth_fence(&fence, 42, true).is_ok());
}

#[test]
fn attempt_fenced_vector_write_rejects_profile_change() {
    let fence = vector_write_fence(
        41,
        Some(IngestVectorWriteFence { attempt_id: Uuid::now_v7(), revision_id: Uuid::now_v7() }),
        false,
    );

    assert!(
        validate_vector_embedding_profile_fence(&fence, "embedding-profile:v1:changed").is_err()
    );
}

#[test]
fn attempt_fenced_vector_write_rejects_authority_loss() {
    assert!(validate_ingest_attempt_write_authority(false).is_err());
}

#[test]
fn advancing_vector_write_requires_exact_source_version() {
    let fence = vector_write_fence(41, None, true);

    assert!(validate_vector_source_truth_fence(&fence, 41, false).is_ok());
    assert!(validate_vector_source_truth_fence(&fence, 42, false).is_err());
}

#[test]
fn revision_scoped_vector_delete_accepts_unrelated_source_advance() {
    // An unrelated library-global advance (a live connector writing another
    // document) must not block deleting the vectors of one revision. The
    // exclusive vector-plane lock, not exact source equality, serializes this
    // destructive mutation against profile promotion and purge.
    assert!(ensure_revision_scoped_vector_delete_source_truth(41, 42).is_ok());
    assert!(ensure_revision_scoped_vector_delete_source_truth(41, 41).is_ok());
}

#[test]
fn revision_scoped_vector_delete_rejects_backwards_and_unsigned_fences() {
    assert!(ensure_revision_scoped_vector_delete_source_truth(41, 40).is_err());
    assert!(ensure_revision_scoped_vector_delete_source_truth(0, 7).is_err());
}

#[test]
fn manifest_reconciliation_requires_exactly_one_prepared_lane() {
    assert!(ensure_single_manifest_row_updated(0).is_err());
    assert!(ensure_single_manifest_row_updated(1).is_ok());
    assert!(ensure_single_manifest_row_updated(2).is_err());
}

#[test]
fn superseded_vector_cursor_accepts_only_an_empty_or_complete_logical_key() {
    assert!(SupersededVectorSweepCursor::default().has_valid_key());

    let complete = SupersededVectorSweepCursor {
        relation_name: Some("knowledge_chunk_vector_d3".to_string()),
        after_chunk_id: Some(Uuid::now_v7()),
        after_revision_id: Some(Uuid::now_v7()),
        after_embedding_model_key: Some("embedding-profile:v1:test".to_string()),
        after_vector_kind: Some("chunk_embedding".to_string()),
        after_freshness_generation: Some(1),
        source_truth_version: Some(7),
    };
    assert!(complete.has_valid_key());

    let partial = SupersededVectorSweepCursor {
        after_chunk_id: Some(Uuid::now_v7()),
        ..SupersededVectorSweepCursor::default()
    };
    assert!(!partial.has_valid_key());
    assert!(
        serde_json::from_value::<SupersededVectorSweepCursor>(serde_json::json!({
            "unknown": true
        }))
        .is_err()
    );
}

#[test]
fn superseded_vector_sweep_progress_preserves_batch_budget_cursor_semantics() {
    let relation_names = vec!["first".to_string(), "second".to_string()];
    let after = SupersededVectorSweepCursor {
        relation_name: Some("first".to_string()),
        after_chunk_id: Some(Uuid::now_v7()),
        after_revision_id: Some(Uuid::now_v7()),
        after_embedding_model_key: Some("embedding-profile:v1:test".to_string()),
        after_vector_kind: Some("chunk_embedding".to_string()),
        after_freshness_generation: Some(1),
        source_truth_version: Some(7),
    };

    assert_eq!(
        superseded_vector_sweep_progress(
            Some(1),
            1,
            SUPERSEDED_VECTOR_DELETE_BATCH_SIZE,
            &SupersededRelationPosition { names: &relation_names, index: 0 },
            Some(&after),
            7,
        )
        .expect("full page should preserve its keyset cursor"),
        SupersededVectorSweepProgress::BatchBudgetExhausted {
            work_remaining: true,
            next_cursor: Some(after),
        }
    );

    assert_eq!(
        superseded_vector_sweep_progress(
            Some(1),
            1,
            1,
            &SupersededRelationPosition { names: &relation_names, index: 0 },
            None,
            8
        )
        .expect("partial page should advance to the next relation"),
        SupersededVectorSweepProgress::BatchBudgetExhausted {
            work_remaining: true,
            next_cursor: Some(SupersededVectorSweepCursor {
                relation_name: Some("second".to_string()),
                source_truth_version: Some(8),
                ..SupersededVectorSweepCursor::default()
            }),
        }
    );
}

#[test]
fn superseded_vector_sweep_progress_distinguishes_completion_from_continuation() {
    let relation_names = vec!["only".to_string()];

    assert_eq!(
        superseded_vector_sweep_progress(
            Some(1),
            1,
            1,
            &SupersededRelationPosition { names: &relation_names, index: 0 },
            None,
            9
        )
        .expect("final partial page should complete the sweep"),
        SupersededVectorSweepProgress::BatchBudgetExhausted {
            work_remaining: false,
            next_cursor: None,
        }
    );
    assert_eq!(
        superseded_vector_sweep_progress(
            None,
            1,
            1,
            &SupersededRelationPosition { names: &relation_names, index: 0 },
            None,
            9
        )
        .expect("unbounded partial page should complete the relation"),
        SupersededVectorSweepProgress::RelationComplete
    );
    assert_eq!(
        superseded_vector_sweep_progress(
            None,
            1,
            SUPERSEDED_VECTOR_DELETE_BATCH_SIZE,
            &SupersededRelationPosition { names: &relation_names, index: 0 },
            None,
            9,
        )
        .expect("unbounded full page should continue the relation"),
        SupersededVectorSweepProgress::ContinueRelation
    );
}

#[test]
fn staging_cleanup_accepts_only_the_opaque_rebuild_key_protocol() {
    let valid = format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
    assert!(validate_vector_rebuild_staging_profile_key(&valid).is_ok());
    assert_eq!(vector_rebuild_staging_source_truth_version(&valid).unwrap(), 0x0123_4567_89ab_cdef,);
    assert!(
        validate_vector_rebuild_staging_source_truth_version(&valid, 0x0123_4567_89ab_cdef,)
            .is_ok()
    );
    assert!(validate_vector_rebuild_staging_source_truth_version(&valid, 1).is_err());
    assert!(
        validate_vector_rebuild_staging_profile_key(&format!(
            "{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}",
            "0".repeat(64)
        ))
        .is_err()
    );
    assert!(
        validate_vector_rebuild_staging_profile_key(
            "embedding-profile:v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_err()
    );
    assert!(
        validate_vector_rebuild_staging_profile_key("embedding-rebuild:v1:0123456789abcdef")
            .is_err()
    );
    assert!(
        validate_vector_rebuild_staging_profile_key(
            "embedding-rebuild:v1:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err()
    );
}

#[test]
fn dimension_claim_reader_accepts_only_exact_embedding_profile_keys() {
    let valid = format!("{EMBEDDING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
    assert!(validate_exact_embedding_profile_key(&valid).is_ok());
    assert!(validate_exact_embedding_profile_key(&Uuid::now_v7().to_string()).is_err());
    assert!(
        validate_exact_embedding_profile_key(
            "embedding-rebuild:v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_err()
    );
    assert!(
        validate_exact_embedding_profile_key(
            "embedding-profile:v1:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err()
    );
}

#[test]
fn dimension_claim_requires_one_positive_storage_safe_matching_relation() {
    assert_eq!(
        validate_exact_profile_dimension_claim(CHUNK_VECTOR_RELATION_PREFIX, &[]).unwrap(),
        None
    );
    assert_eq!(
        validate_exact_profile_dimension_claim(
            CHUNK_VECTOR_RELATION_PREFIX,
            &[(3, "knowledge_chunk_vector_d3".to_string())],
        )
        .unwrap(),
        Some(3)
    );
    assert!(
        validate_exact_profile_dimension_claim(
            CHUNK_VECTOR_RELATION_PREFIX,
            &[(0, "knowledge_chunk_vector_d0".to_string())],
        )
        .is_err()
    );
    assert!(
        validate_exact_profile_dimension_claim(
            CHUNK_VECTOR_RELATION_PREFIX,
            &[(4_001, "knowledge_chunk_vector_d4001".to_string())],
        )
        .is_err()
    );
    assert!(
        validate_exact_profile_dimension_claim(
            CHUNK_VECTOR_RELATION_PREFIX,
            &[(3, "knowledge_chunk_vector_d4".to_string())],
        )
        .is_err()
    );
    assert!(
        validate_exact_profile_dimension_claim(
            CHUNK_VECTOR_RELATION_PREFIX,
            &[
                (3, "knowledge_chunk_vector_d3".to_string()),
                (4, "knowledge_chunk_vector_d4".to_string()),
            ],
        )
        .is_err()
    );
}
