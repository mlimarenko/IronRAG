use super::test_support::*;
use super::*;

/// Selected document graph-evidence line as the retrieval lane builds it: the
/// rendered header plus the passage body the producer formatted into it.
fn graph_evidence_line(header: &str, body: &str) -> SelectedGraphContextLine {
    SelectedGraphContextLine::passage(format!("{header}\n{body}"), body.to_string())
}

#[test]
fn exact_version_query_does_not_use_latest_identity_fallback() {
    let mut ir = latest_source_slice_ir(5);
    ir.source_slice = None;
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "1.0.2".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Version,
    }];
    let chunks = vec![release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        10.0,
        "Sample Subject Release 1.0.2",
        "exact release detail",
    )];

    let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

    assert!(answer.is_none());
}

#[test]
fn exact_version_change_answer_uses_matching_graph_evidence_bullets() {
    let lines = vec![
        graph_evidence_line(
            "[graph-evidence target=\"Version 1.2.2\"]",
            "Version 1.2.2\n- Older item",
        ),
        graph_evidence_line(
            "[graph-evidence target=\"Version 1.2.3\"]",
            "Version 1.2.3 - Sample Subject\n\nChanges\n\n- Added indexed lookup by suffix\n- Added `pricedocid` to `documents.goodsitem`\n- Updated monitor counters",
        ),
        graph_evidence_line(
            "[graph-evidence target=\"Sample Subject --has_property--> 1.2.3\"]",
            "Related guide\n- Adjacent workflow note",
        ),
    ];

    let answer = build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines)
        .expect("exact version graph answer");

    assert!(answer.contains("Version 1.2.3 - Sample Subject"));
    assert!(answer.contains("Added indexed lookup by suffix"));
    assert!(answer.contains("`pricedocid`"));
    assert!(!answer.contains("Older item"));
    assert!(!answer.contains("Adjacent workflow note"));
}

#[test]
fn exact_version_change_answer_falls_back_to_matching_context_chunks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        release_identity_chunk(
            document_id,
            revision_id,
            0,
            10.0,
            "Sample Subject Version 1.2.3",
            "# Version 1.2.3\n\n- Added operator audit\n- Added retry metric",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Version 1.2.2",
            "# Version 1.2.2\n\n- Older item",
        ),
    ];

    let answer =
        build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &chunks, &[])
            .expect("exact version chunk answer");

    assert!(answer.contains("Sample Subject Version 1.2.3"));
    assert!(answer.contains("Added operator audit"));
    assert!(answer.contains("Added retry metric"));
    assert!(!answer.contains("Older item"));
}

#[test]
fn exact_version_change_answer_does_not_match_version_substrings() {
    let lines = vec![
        graph_evidence_line(
            "[graph-evidence target=\"Version 1.2.30\"]",
            "Version 1.2.30\n- Wrong patch line\n- Wrong second line",
        ),
        graph_evidence_line(
            "[graph-evidence target=\"Version 21.2.3\"]",
            "Version 21.2.3\n- Wrong major line\n- Wrong second line",
        ),
    ];

    let answer = build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines);

    assert!(answer.is_none());
}

#[test]
fn exact_version_change_answer_reads_evidence_not_the_rendered_header() {
    let lines = [graph_evidence_line(
        "[graph-evidence target=\"Version 1.2.3\"]",
        "Version 1.2.2\n- Older item\n- Second older item",
    )];

    let answer = build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines);

    assert!(answer.is_none());
}

#[test]
fn exact_version_change_answer_ignores_matched_entity_summary_text() {
    let body = "Version 1.2.3 - Sample Subject\n\nChanges\n\n- Added indexed lookup by suffix\n- Updated monitor counters";
    let matched_entity = SelectedGraphContextLine::matched_entity(
        format!(
            "[graph-node] target_match=explicit evidence: {body} | entity_hint: Sample Subject (artifact)"
        ),
        Some(body.to_string()),
    );
    // Same summary text as a document passage is eligible.
    let passage = graph_evidence_line("[graph-evidence target=\"Version 1.2.3\"]", body);

    let from_entity = build_exact_version_change_summary_answer(
        &exact_version_ir("1.2.3"),
        &[],
        &[matched_entity],
    );
    let from_passage =
        build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &[passage])
            .expect("document passage keeps the exact-version shortcut");

    assert!(from_entity.is_none());
    assert!(from_passage.contains("Added indexed lookup by suffix"));
}

#[test]
fn multi_document_description_with_exact_version_is_not_a_changelog() {
    let mut ir = exact_version_ir("v1");
    ir.scope = crate::domains::query_ir::QueryScope::MultiDocument;
    ir.target_types.extend([
        QueryTargetKind::SoftwareModule,
        QueryTargetKind::State,
        QueryTargetKind::Transition,
        QueryTargetKind::Artifact,
    ]);
    let lines = [graph_evidence_line(
        "[graph-evidence]",
        "Version v1 cleanup\n- 11:30 UTC: Stopped earlier deployment\n- 12:10 UTC: Observed new service returning ready",
    )];
    let chunks = [release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        10.0,
        "Version v1 readiness record",
        "- Retained the previous data volume\n- Checked the public health endpoint",
    )];

    assert!(build_exact_version_change_summary_answer(&ir, &chunks, &lines).is_none());
    assert!(build_exact_version_change_summary_answer(&ir, &chunks, &[]).is_none());
}

#[test]
fn exact_version_change_answer_groups_chunks_by_document_id_before_label() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let shared_label = "Sample Subject Version 1.2.3";
    let chunks = vec![
        release_identity_chunk(
            document_id,
            revision_id,
            0,
            10.0,
            shared_label,
            "# Version 1.2.3\n\n- Added operator audit",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            1,
            10.0,
            shared_label,
            "# Version 1.2.3\n\n- Adjacent one-line note",
        ),
        release_identity_chunk(
            document_id,
            revision_id,
            2,
            10.0,
            shared_label,
            "# Version 1.2.3\n\n- Added retry metric",
        ),
    ];

    let answer =
        build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &chunks, &[])
            .expect("grouped exact version answer");

    assert!(answer.contains("Added operator audit"));
    assert!(answer.contains("Added retry metric"));
}
