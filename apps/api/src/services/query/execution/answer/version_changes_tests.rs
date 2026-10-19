use super::test_support::*;
use super::*;

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
            "[graph-evidence target=\"Version 1.2.2\"]\nVersion 1.2.2\n- Older item"
                .to_string(),
            "[graph-evidence target=\"Version 1.2.3\"]\nVersion 1.2.3 - Sample Subject\n\nChanges\n\n- Added indexed lookup by suffix\n- Added `pricedocid` to `documents.goodsitem`\n- Updated monitor counters"
                .to_string(),
            "[graph-evidence target=\"Sample Subject --has_property--> 1.2.3\"]\nRelated guide\n- Adjacent workflow note"
                .to_string(),
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
            "[graph-evidence target=\"Version 1.2.30\"]\nVersion 1.2.30\n- Wrong patch line\n- Wrong second line"
                .to_string(),
            "[graph-evidence target=\"Version 21.2.3\"]\nVersion 21.2.3\n- Wrong major line\n- Wrong second line"
                .to_string(),
        ];

    let answer = build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines);

    assert!(answer.is_none());
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
