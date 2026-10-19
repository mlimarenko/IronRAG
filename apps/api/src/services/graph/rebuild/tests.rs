use super::revision_projection::{
    RevisionReconcileProjectionPlan, is_graph_reconcile_chunk_text_eligible,
    preserved_snapshot_terminal_state, revision_reconcile_projection_plan, union_targeted_ids,
};
use super::*;

#[test]
fn malformed_persisted_candidate_contract_fails_closed() {
    let error = repaired_graph_extraction_candidates(serde_json::json!({
        "entities": { "unexpected": true },
        "relations": []
    }))
    .expect_err("malformed persisted candidates must not become an empty successful graph");

    assert!(error.to_string().contains("candidate contract"));
}

#[test]
fn rebuild_rejects_catastrophic_baseline_coverage_loss() {
    let baseline = (1..=100).map(Uuid::from_u128).collect::<BTreeSet<_>>();
    let projected = (1..=9).map(Uuid::from_u128).collect::<BTreeSet<_>>();
    let retained = projected.intersection(&baseline).count();

    assert_eq!(retained, 9);
    assert!(!rebuild_coverage_meets_publication_floor(retained, baseline.len()));
}

#[test]
fn rebuild_accepts_projection_at_retention_floor() {
    let baseline = (1..=100).map(Uuid::from_u128).collect::<BTreeSet<_>>();
    let projected = (1..=MIN_REBUILD_PUBLICATION_FLOOR_PERCENT as u128)
        .map(Uuid::from_u128)
        .collect::<BTreeSet<_>>();
    let retained = projected.intersection(&baseline).count();

    assert!(rebuild_coverage_meets_publication_floor(retained, baseline.len()));
}

#[test]
fn rebuild_rejects_sparse_ready_coverage_even_when_baseline_is_retained() {
    let ready = ready_graph_documents((1..=100).map(|value| (value, (value <= 9).then_some(1))));
    let baseline = (1..=10).map(Uuid::from_u128).collect::<BTreeSet<_>>();
    let projected = baseline.clone();

    assert!(!rebuild_coverage_counts(&projected, &baseline, &ready).is_publishable());
}

#[test]
fn first_projection_rejects_nine_completions_for_one_hundred_ready_documents() {
    let ready = ready_graph_documents((1..=100).map(|value| (value, (value <= 9).then_some(1))));
    let projected = (1..=9).map(Uuid::from_u128).collect::<BTreeSet<_>>();

    assert!(!rebuild_coverage_counts(&projected, &BTreeSet::new(), &ready).is_publishable());
}

#[test]
fn sparse_completed_extractions_publish_when_every_positive_document_is_projected() {
    let ready =
        ready_graph_documents((1..=100).map(|value| (value, Some(usize::from(value <= 9)))));
    let positive_completions = (1..=9).map(Uuid::from_u128).collect::<BTreeSet<_>>();

    assert!(
        rebuild_coverage_counts(&positive_completions, &BTreeSet::new(), &ready).is_publishable()
    );
}

#[test]
fn first_projection_accepts_ready_coverage_at_publication_floor() {
    let ready = ready_graph_documents((1..=100).map(|value| {
        (value, (value <= MIN_REBUILD_PUBLICATION_FLOOR_PERCENT as u128).then_some(1))
    }));
    let projected = (1..=MIN_REBUILD_PUBLICATION_FLOOR_PERCENT as u128)
        .map(Uuid::from_u128)
        .collect::<BTreeSet<_>>();

    assert!(rebuild_coverage_counts(&projected, &BTreeSet::new(), &ready).is_publishable());
}

#[test]
fn rebuild_rejects_nine_projected_for_one_hundred_positive_completions() {
    let ready = ready_graph_documents((1..=100).map(|value| (value, Some(1))));
    let projected = (1..=9).map(Uuid::from_u128).collect::<BTreeSet<_>>();

    assert!(!rebuild_coverage_counts(&projected, &BTreeSet::new(), &ready).is_publishable());
}

#[test]
fn ready_graph_document_requires_readable_revision_to_equal_active_revision() {
    let active_revision_id = Uuid::now_v7();
    let matching = document_head(Some(active_revision_id), Some(active_revision_id));
    let different = document_head(Some(active_revision_id), Some(Uuid::now_v7()));
    let unreadable = document_head(Some(active_revision_id), None);

    assert_eq!(active_readable_revision_id(&matching), Some(active_revision_id));
    assert_eq!(active_readable_revision_id(&different), None);
    assert_eq!(active_readable_revision_id(&unreadable), None);
}

#[test]
fn rebuild_uses_more_complete_previous_projection_as_baseline() {
    let active_document_id = Uuid::from_u128(1);
    let previous_only_document_id = Uuid::from_u128(2);
    let deleted_document_id = Uuid::from_u128(3);
    let ready_documents = [active_document_id, previous_only_document_id]
        .into_iter()
        .map(|document_id| (document_id, ready_graph_document(None)))
        .collect::<BTreeMap<_, _>>();

    let baseline = select_rebuild_baseline_document_ids(
        [active_document_id].into_iter().collect(),
        [active_document_id, previous_only_document_id, deleted_document_id].into_iter().collect(),
        &ready_documents,
    );

    assert_eq!(baseline, [active_document_id, previous_only_document_id].into_iter().collect());
}

#[test]
fn current_zero_completion_retires_stale_previous_baseline_without_recovery() {
    let positive_document_id = Uuid::from_u128(1);
    let zero_contribution_document_id = Uuid::from_u128(2);
    let ready_documents = [
        (positive_document_id, ready_graph_document(Some(1))),
        (zero_contribution_document_id, ready_graph_document(Some(0))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let projected_document_ids = [positive_document_id].into_iter().collect::<BTreeSet<_>>();

    // The previous projection still has a document node from an older
    // revision. The exact current completion marker is authoritative: a zero
    // contribution means the stale node must disappear, not become a
    // permanent baseline requirement.
    let baseline = select_rebuild_baseline_document_ids(
        projected_document_ids.clone(),
        [positive_document_id, zero_contribution_document_id].into_iter().collect(),
        &ready_documents,
    );

    assert_eq!(baseline, projected_document_ids);
    assert!(
        rebuild_coverage_counts(&projected_document_ids, &baseline, &ready_documents)
            .is_publishable(),
        "the rebuilt projection may publish after retiring the stale zero-contribution node"
    );
    assert!(
        missing_reextract_candidates(&ready_documents, &projected_document_ids).is_empty(),
        "a completed zero-contribution revision must not enter an impossible recovery loop"
    );
}

#[test]
fn sparse_rebuild_recovers_missing_completion_or_positive_projection_only() {
    let retained_document_id = Uuid::from_u128(1);
    let missing_positive_document_id = Uuid::from_u128(2);
    let missing_completion_document_id = Uuid::from_u128(3);
    let zero_contribution_document_id = Uuid::from_u128(4);
    let missing_positive = ready_graph_document(Some(1));
    let missing_completion = ready_graph_document(None);
    let ready_documents = [
        (retained_document_id, ready_graph_document(Some(1))),
        (missing_positive_document_id, missing_positive),
        (missing_completion_document_id, missing_completion),
        (zero_contribution_document_id, ready_graph_document(Some(0))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let projected = [retained_document_id].into_iter().collect();

    let candidates = missing_reextract_candidates(&ready_documents, &projected);

    assert_eq!(
        candidates,
        vec![
            GraphReextractCandidate {
                workspace_id: missing_positive.workspace_id,
                document_id: missing_positive_document_id,
                revision_id: missing_positive.revision_id,
            },
            GraphReextractCandidate {
                workspace_id: missing_completion.workspace_id,
                document_id: missing_completion_document_id,
                revision_id: missing_completion.revision_id,
            },
        ]
    );
}

#[test]
fn sparse_rebuild_bounds_recovery_candidates() {
    let ready_documents = (1..=400)
        .map(|value| (Uuid::from_u128(value), ready_graph_document((value > 300).then_some(0))))
        .collect::<BTreeMap<_, _>>();

    let candidates = missing_reextract_candidates(&ready_documents, &BTreeSet::new());

    assert_eq!(candidates.len(), usize::try_from(REEXTRACT_MAX_BATCH_SIZE).unwrap_or(usize::MAX));
    assert!(candidates.iter().all(|candidate| candidate.document_id.as_u128() <= 300));
}

#[test]
fn re_revision_with_changes_takes_targeted_supersede_path() {
    // A re-revision on a populated library (the OOM scenario): there is a
    // previous revision and the existing graph is non-empty. Even though the
    // old full-rebuild trigger fired here, we now route to the bounded
    // targeted supersede path instead of a library-wide rebuild.
    let plan = revision_reconcile_projection_plan(
        true,
        3,
        false,
        true,
        &[Uuid::now_v7()],
        &[Uuid::now_v7()],
    );
    assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedSupersede);
    assert!(plan.broad_summary_refresh());
}

#[test]
fn re_revision_pruning_only_still_targeted_supersede() {
    // Content-shrinking re-revision: the new revision contributes nothing
    // (graph_contribution_count == 0) but the superseded revision left
    // orphan targets that must be pruned. Must NOT fall through to preserve.
    let plan = revision_reconcile_projection_plan(true, 0, false, true, &[Uuid::now_v7()], &[]);
    assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedSupersede);
}

#[test]
fn re_revision_empty_union_preserves_instead_of_full_rebuild() {
    // Genuine no-op re-revision: nothing merged, nothing superseded. The
    // empty union must route to preserve, never silently into the full
    // (OOM) rebuild.
    let plan = revision_reconcile_projection_plan(true, 0, false, true, &[], &[]);
    assert_eq!(plan, RevisionReconcileProjectionPlan::Preserve);
}

#[test]
fn re_revision_empty_union_without_snapshot_is_empty() {
    let plan = revision_reconcile_projection_plan(true, 0, false, false, &[], &[]);
    assert_eq!(plan, RevisionReconcileProjectionPlan::Empty);
}

#[test]
fn first_revision_on_empty_library_is_full() {
    // First-ever graph for the library: full projection is cheap on an
    // empty/new graph.
    let plan = revision_reconcile_projection_plan(
        false,
        5,
        true,
        false,
        &[Uuid::now_v7()],
        &[Uuid::now_v7()],
    );
    assert_eq!(plan, RevisionReconcileProjectionPlan::Full);
}

#[test]
fn first_revision_on_populated_library_is_targeted_fresh() {
    let plan = revision_reconcile_projection_plan(
        false,
        5,
        false,
        true,
        &[Uuid::now_v7()],
        &[Uuid::now_v7()],
    );
    assert_eq!(plan, RevisionReconcileProjectionPlan::TargetedFresh);
    assert!(!plan.broad_summary_refresh());
}

#[test]
fn first_revision_no_contribution_preserves() {
    let plan = revision_reconcile_projection_plan(false, 0, false, true, &[], &[]);
    assert_eq!(plan, RevisionReconcileProjectionPlan::Preserve);
}

#[test]
fn preserving_failed_snapshot_keeps_failure_state_and_error() {
    let mut snapshot = snapshot_row(Uuid::now_v7(), "failed", 7, 12, 34);
    snapshot.last_error_message = Some("synthetic graph-store failure".to_string());

    let preserved = preserved_snapshot_terminal_state(&snapshot);

    assert_eq!(preserved.graph_status, "failed");
    assert_eq!(preserved.last_error_message, Some("synthetic graph-store failure"));
}

#[test]
fn preserving_healthy_snapshot_derives_ready_or_empty_from_counts() {
    let ready = snapshot_row(Uuid::now_v7(), "ready", 7, 12, 34);
    let empty = snapshot_row(Uuid::now_v7(), "empty", 7, 0, 0);

    assert_eq!(preserved_snapshot_terminal_state(&ready).graph_status, GRAPH_STATUS_READY);
    assert_eq!(preserved_snapshot_terminal_state(&empty).graph_status, GRAPH_STATUS_EMPTY);
}

#[test]
fn union_targeted_ids_dedups_and_merges() {
    let shared = Uuid::now_v7();
    let only_changed = Uuid::now_v7();
    let only_superseded = Uuid::now_v7();
    let superseded: BTreeSet<Uuid> = [shared, only_superseded].into_iter().collect();
    let union = union_targeted_ids(&[shared, only_changed], &superseded);
    assert_eq!(union.len(), 3);
    assert!(union.contains(&shared));
    assert!(union.contains(&only_changed));
    assert!(union.contains(&only_superseded));
}

#[test]
fn empty_rebuild_conflicts_with_live_snapshot_without_authoritative_input() {
    let library_id = Uuid::now_v7();
    let snapshot = snapshot_row(library_id, GRAPH_STATUS_READY, 7, 12, 34);
    let ready_documents = BTreeMap::new();
    let coverage = rebuild_coverage_counts(&BTreeSet::new(), &BTreeSet::new(), &ready_documents);

    let error = empty_rebuild_conflict(
        library_id,
        Some(&snapshot),
        &ready_documents,
        coverage,
        "no source",
    )
    .expect("live graph must reject an empty rebuild with no authoritative current input");

    assert!(matches!(error, GraphServiceError::StateConflict { .. }));
    assert!(error.to_string().contains("projection 7"));
    assert!(error.to_string().contains("left unchanged"));
}

#[test]
fn empty_rebuild_allows_authoritative_zero_completions_over_live_snapshot() {
    let library_id = Uuid::now_v7();
    let snapshot = snapshot_row(library_id, GRAPH_STATUS_READY, 7, 12, 34);
    let ready_documents = ready_graph_documents([(1, Some(0)), (2, Some(0))]);
    let coverage = rebuild_coverage_counts(&BTreeSet::new(), &BTreeSet::new(), &ready_documents);

    assert!(authoritative_zero_rebuild_is_proven(&ready_documents, coverage));
    assert!(
        empty_rebuild_conflict(
            library_id,
            Some(&snapshot),
            &ready_documents,
            coverage,
            "no admitted graph rows",
        )
        .is_none()
    );
}

#[test]
fn empty_rebuild_rejects_zero_marker_without_exact_completion_attempt() {
    let mut document = ready_graph_document(Some(0));
    document.activated_by_attempt_id = None;
    let ready_documents = [(Uuid::from_u128(1), document)].into_iter().collect::<BTreeMap<_, _>>();

    assert_incomplete_zero_evidence_is_refused(&ready_documents);
}

#[test]
fn empty_rebuild_rejects_incomplete_zero_evidence_above_coverage_floor() {
    let ready_documents = (1..=100)
        .map(|document_id| {
            let mut document = ready_graph_document((document_id < 100).then_some(0));
            if document_id == 100 {
                document.activated_by_attempt_id = None;
            }
            (Uuid::from_u128(document_id), document)
        })
        .collect::<BTreeMap<_, _>>();

    assert_incomplete_zero_evidence_is_refused(&ready_documents);
}

fn assert_incomplete_zero_evidence_is_refused(
    ready_documents: &BTreeMap<Uuid, ReadyGraphDocument>,
) {
    let library_id = Uuid::now_v7();
    let snapshot = snapshot_row(library_id, GRAPH_STATUS_READY, 7, 12, 34);
    let coverage = rebuild_coverage_counts(&BTreeSet::new(), &BTreeSet::new(), ready_documents);

    assert!(coverage.is_publishable());
    assert!(!authoritative_zero_rebuild_is_proven(ready_documents, coverage));
    assert!(
        empty_rebuild_conflict(
            library_id,
            Some(&snapshot),
            ready_documents,
            coverage,
            "no admitted graph rows",
        )
        .is_some()
    );
}

#[test]
fn empty_rebuild_allows_empty_or_absent_snapshot() {
    let library_id = Uuid::now_v7();
    let empty_snapshot = snapshot_row(library_id, "empty", 3, 0, 0);
    let ready_documents = BTreeMap::new();
    let coverage = rebuild_coverage_counts(&BTreeSet::new(), &BTreeSet::new(), &ready_documents);

    assert!(
        empty_rebuild_conflict(library_id, None, &ready_documents, coverage, "no source").is_none()
    );
    assert!(
        empty_rebuild_conflict(
            library_id,
            Some(&empty_snapshot),
            &ready_documents,
            coverage,
            "no source",
        )
        .is_none()
    );
}

#[test]
fn graph_reconcile_rejects_low_confidence_current_chunk_text() {
    let text = concat!(
        "overview status alpha beta gamma. ",
        "<!-- formula-not-decoded --> ",
        "abCD4efGH hiJKlmNO pQrST uvWXyZab. ",
        "cdEFGh3Ij klMNOprs tuVWxyZq mnOPqRst."
    );

    assert!(!is_graph_reconcile_chunk_text_eligible(text));
}

#[test]
fn graph_reconcile_accepts_code_like_current_chunk_text() {
    let text = concat!(
        "POST /api/v1/projects getProjectById renderHTMLNode ",
        "AUTH_TOKEN_TIMEOUT_MS status_code retry_count"
    );

    assert!(is_graph_reconcile_chunk_text_eligible(text));
}

fn snapshot_row(
    library_id: Uuid,
    graph_status: &str,
    projection_version: i64,
    node_count: i32,
    edge_count: i32,
) -> RuntimeGraphSnapshotRow {
    RuntimeGraphSnapshotRow {
        library_id,
        graph_status: graph_status.to_string(),
        projection_version,
        topology_generation: 1,
        node_count,
        edge_count,
        provenance_coverage_percent: Some(100.0),
        last_built_at: None,
        last_error_message: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn ready_graph_documents(
    graph_contribution_counts: impl IntoIterator<Item = (u128, Option<usize>)>,
) -> BTreeMap<Uuid, ReadyGraphDocument> {
    graph_contribution_counts
        .into_iter()
        .map(|(document_id, graph_contribution_count)| {
            (Uuid::from_u128(document_id), ready_graph_document(graph_contribution_count))
        })
        .collect()
}

fn ready_graph_document(graph_contribution_count: Option<usize>) -> ReadyGraphDocument {
    ReadyGraphDocument {
        workspace_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        activated_by_attempt_id: Some(Uuid::now_v7()),
        graph_contribution_count,
    }
}

fn document_head(
    active_revision_id: Option<Uuid>,
    readable_revision_id: Option<Uuid>,
) -> content_document_head_repository::ContentDocumentHeadRow {
    content_document_head_repository::ContentDocumentHeadRow {
        visibility_intent_id: None,
        document_id: Uuid::now_v7(),
        active_revision_id,
        readable_revision_id,
        latest_mutation_id: None,
        latest_successful_attempt_id: None,
        head_updated_at: Utc::now(),
        document_summary: None,
    }
}
