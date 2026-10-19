use super::*;

#[test]
fn restore_library_data_plan_allows_reject_into_empty_target() {
    assert_eq!(
        plan_restore_library_data(
            &[IncludeKind::LibraryData],
            OverwriteMode::Reject,
            TargetRestoreFootprint::Empty,
        ),
        RestoreLibraryDataAction::ImportIntoEmptyTarget
    );
    assert_eq!(
        plan_restore_library_data(
            &[IncludeKind::LibraryData],
            OverwriteMode::Reject,
            TargetRestoreFootprint::Populated,
        ),
        RestoreLibraryDataAction::RejectPopulatedTarget
    );
    assert_eq!(
        plan_restore_library_data(
            &[IncludeKind::LibraryData],
            OverwriteMode::Replace,
            TargetRestoreFootprint::Populated,
        ),
        RestoreLibraryDataAction::ReplaceTarget
    );
    assert_eq!(
        plan_restore_library_data(
            &[IncludeKind::AiConfig],
            OverwriteMode::Replace,
            TargetRestoreFootprint::Populated,
        ),
        RestoreLibraryDataAction::Skip
    );
}

#[tokio::test]
async fn reject_populated_target_has_no_pre_restore_side_effect() {
    let side_effect_started = std::cell::Cell::new(false);

    let result = run_restore_preflight_before_side_effects(
        Uuid::now_v7(),
        RestoreLibraryDataAction::RejectPopulatedTarget,
        || async {
            side_effect_started.set(true);
            Ok(())
        },
    )
    .await;

    assert!(result.is_err());
    assert!(!side_effect_started.get());
}

#[tokio::test]
async fn allowed_restore_action_runs_the_preflight_side_effect() {
    let side_effect_started = std::cell::Cell::new(false);

    let result = run_restore_preflight_before_side_effects(
        Uuid::now_v7(),
        RestoreLibraryDataAction::ReplaceTarget,
        || async {
            side_effect_started.set(true);
            Ok(())
        },
    )
    .await;

    assert!(result.is_ok());
    assert!(side_effect_started.get());
}

#[test]
fn restore_footprint_clauses_follow_declared_snapshot_tables() {
    let content_document = postgres_restore_footprint_clause("content_document")
        .unwrap()
        .expect("content_document carries direct library scope");
    assert!(content_document.contains("\"content_document\""));
    assert!(content_document.contains("library_id = $1"));

    let content_chunk = postgres_restore_footprint_clause("content_chunk")
        .unwrap()
        .expect("content_chunk is scoped through content_revision");
    assert!(content_chunk.contains("JOIN content_revision"));
    assert!(content_chunk.contains("r.library_id = $1"));

    let vector_manifest = postgres_restore_footprint_clause("knowledge_vector_relation_manifest")
        .unwrap()
        .expect("vector manifest rows make the target non-empty");
    assert!(vector_manifest.contains("\"knowledge_vector_relation_manifest\""));
}

#[test]
fn catalog_library_snapshot_update_preserves_missing_default_columns() {
    let sql = catalog_library_snapshot_update_sql();
    assert!(
        sql.contains("retrieval_config = COALESCE(row.retrieval_config, target.retrieval_config)")
    );
    assert!(
        sql.contains(
            "web_ingest_policy = COALESCE(row.web_ingest_policy, target.web_ingest_policy)"
        )
    );
    assert!(sql.contains("retrieval_config jsonb"));
    assert!(sql.contains("FROM jsonb_to_recordset($1)"));
}

#[test]
fn restore_source_truth_floor_rejects_lower_or_same_imported_epoch() {
    let locked_target_version = 41;
    let minimum_committed_version =
        minimum_source_truth_version_after_restore(locked_target_version)
            .expect("ordinary source version has a successor");

    for imported_version in [1, locked_target_version] {
        assert!(
            minimum_committed_version > imported_version,
            "a lower or equal archive epoch must not become the committed target epoch",
        );
    }

    let sql = advance_restored_library_source_truth_version_sql();
    assert!(sql.contains("coalesce(source_truth_version, 0) + 1"));
    assert!(sql.contains("greatest"));
    assert!(sql.contains("source_truth_version >= $2"));
}
