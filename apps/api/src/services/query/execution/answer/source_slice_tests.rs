use super::test_support::*;
use super::*;

#[test]
fn ordered_source_slice_answer_yields_mixed_release_shape_to_synthesis() {
    let mut query_ir = source_slice_ir(3);
    query_ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
    let valid = source_unit(1, "[unit_id=release-1] Version 4.2.0\n- Added a neutral capability");
    let table_summary = source_unit(
        2,
        "[unit_id=summary-1] Table Summary | Sheet: inventory | Column: Discount | Value Kind: numeric | Value Shape: identifier | Aggregation Priority: 3 | Row Count: 4 | Non-empty Count: 4 | Distinct Count: 4 | Average: 20 | Min: 10 | Max: 30",
    );
    let unrelated =
        source_unit(3, "[unit_id=guide-1] General upgrade guidance without a release record");

    let answer =
        build_ordered_source_slice_answer(&query_ir, &[valid, table_summary, unrelated], &[]);

    assert!(answer.is_none(), "mixed-shape inventory must continue to synthesis");
}

#[test]
fn ordered_source_slice_answer_yields_release_shaped_upgrade_guides_to_synthesis() {
    let upgrade_2604 = release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        1,
        20.0,
        "Upgrade guide for OS 26.04",
        "Version 26.04\n- Back up the machine before starting the upgrade",
    );
    let upgrade_2510 = release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        2,
        10.0,
        "Upgrade guide for OS 25.10",
        "Version 25.10\n- Check the supported upgrade path before proceeding",
    );

    let answer = build_ordered_source_slice_answer(
        &latest_source_slice_ir(2),
        &[],
        &[upgrade_2604, upgrade_2510],
    );

    assert!(answer.is_none(), "release-shaped guides must continue to synthesis");
}

#[test]
fn ordered_source_slice_answer_dedupes_normalized_bodies_and_keeps_fullest_label() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut short_label = source_unit(
        1,
        "[unit_id=record-a occurred_at=2026-02-01T00:00:00Z] Repeated neutral payload",
    );
    short_label.document_id = document_id;
    short_label.revision_id = revision_id;
    short_label.document_label = "Brief record".to_string();
    let mut full_label = source_unit(
        2,
        "[unit_id=record-b occurred_at=2026-02-02T00:00:00Z]   repeated   NEUTRAL payload ",
    );
    full_label.document_id = document_id;
    full_label.revision_id = revision_id;
    full_label.document_label = "Full descriptive record label".to_string();

    let source_units = vec![short_label, full_label];
    let answer =
        build_ordered_source_slice_answer(&source_slice_ir(2), &source_units, &source_units)
            .expect("deduplicated record answer");

    assert_eq!(answer.unit_count, 1);
    assert_eq!(answer.answer.to_lowercase().matches("repeated neutral payload").count(), 1);
    assert!(answer.answer.contains("Full descriptive record label"), "{}", answer.answer);
    assert!(!answer.answer.contains("Brief record"), "{}", answer.answer);
}

#[test]
fn ordered_source_slice_answer_hides_internal_table_summary_fields() {
    let summary = source_unit(
        1,
        "[unit_id=record-1 occurred_at=2026-02-01T00:00:00Z] Table Summary | Sheet: inventory | Table: status | Column: State | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 4 | Non-empty Count: 4 | Distinct Count: 2 | Most Frequent Count: 3 | Most Frequent Tie Count: 1 | Most Frequent Values: active",
    );

    let answer = build_ordered_source_slice_answer(
        &source_slice_ir(1),
        std::slice::from_ref(&summary),
        std::slice::from_ref(&summary),
    )
    .expect("human table-summary answer");

    assert!(answer.answer.contains("Sheet: inventory"), "{}", answer.answer);
    assert!(answer.answer.contains("Table: status"), "{}", answer.answer);
    assert!(answer.answer.contains("Column: State"), "{}", answer.answer);
    assert!(answer.answer.contains("Most Frequent Values: active"), "{}", answer.answer);
    assert!(!answer.answer.contains("Table Summary |"), "{}", answer.answer);
    assert!(!answer.answer.contains("Value Kind:"), "{}", answer.answer);
    assert!(!answer.answer.contains("Aggregation Priority:"), "{}", answer.answer);
}

#[test]
fn ordered_source_slice_answer_keeps_valid_record_inventory() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut later = source_unit(
        2,
        "[unit_ordinal=2 occurred_at=2026-02-02T00:00:00Z actor_label=Recorder] later record",
    );
    later.document_id = document_id;
    later.revision_id = revision_id;
    let mut earlier = source_unit(
        1,
        "[unit_ordinal=1 occurred_at=2026-02-01T00:00:00Z actor_label=Recorder] earlier record",
    );
    earlier.document_id = document_id;
    earlier.revision_id = revision_id;

    let source_units = vec![later, earlier];
    let answer =
        build_ordered_source_slice_answer(&source_slice_ir(2), &source_units, &source_units)
            .expect("valid record inventory answer");

    assert_eq!(answer.unit_count, 2);
    assert!(answer.answer.starts_with("`records.jsonl` - 2/2 source units returned."));
    assert!(
        answer.answer.find("earlier record").unwrap() < answer.answer.find("later record").unwrap()
    );
}

fn ranked_synthesis_corpus() -> (RuntimeMatchedChunk, Vec<RuntimeMatchedChunk>) {
    let prose_document_id = Uuid::now_v7();
    let prose_revision_id = Uuid::now_v7();
    let mut prose_changelog = release_relevance_chunk(
        prose_document_id,
        prose_revision_id,
        0,
        100.0,
        "Prose changelog",
        "Release notes with narrative change descriptions",
    );
    prose_changelog.score_kind = RuntimeChunkScoreKind::Relevance;

    let table_documents = [("Release inventory A", 90.0), ("Release inventory B", 80.0)];
    let record_units = table_documents
        .into_iter()
        .enumerate()
        .map(|(index, (label, score))| {
            let mut unit = source_unit(
                index as i32,
                &format!("[unit_id=row-{index}] structured release row {index}"),
            );
            unit.document_id = Uuid::now_v7();
            unit.revision_id = Uuid::now_v7();
            unit.document_label = label.to_string();
            unit.score = Some(score);
            unit
        })
        .collect();
    (prose_changelog, record_units)
}

#[test]
fn ordered_source_slice_answer_rejects_lower_ranked_record_containers() {
    let (prose_changelog, record_units) = ranked_synthesis_corpus();
    let mut ranked_context = vec![prose_changelog];
    ranked_context.extend(record_units.iter().cloned());

    let answer =
        build_ordered_source_slice_answer(&source_slice_ir(2), &record_units, &ranked_context);

    assert!(answer.is_none(), "lower-ranked record documents must continue to synthesis");
}

#[test]
fn ordered_source_slice_answer_rejects_context_only_record_units() {
    let (prose_changelog, record_units) = ranked_synthesis_corpus();
    let mut fallback_context = vec![prose_changelog];
    fallback_context.extend(record_units);

    let answer = build_ordered_source_slice_answer(&source_slice_ir(2), &[], &fallback_context);

    assert!(answer.is_none(), "fallback context must continue to synthesis");
}

#[test]
fn ordered_source_slice_answer_keeps_top_ranked_record_container() {
    let record_document_id = Uuid::now_v7();
    let record_revision_id = Uuid::now_v7();
    let mut first = source_unit(1, "[unit_id=row-1] first inventory row");
    first.document_id = record_document_id;
    first.revision_id = record_revision_id;
    first.document_label = "Ranked inventory".to_string();
    first.score = Some(100.0);
    let mut second = source_unit(2, "[unit_id=row-2] second inventory row");
    second.document_id = record_document_id;
    second.revision_id = record_revision_id;
    second.document_label = "Ranked inventory".to_string();
    second.score = Some(99.0);
    let source_units = vec![first, second];
    let mut lower_ranked_prose = evidence_chunk(0, Some("paragraph"), "Narrative appendix");
    lower_ranked_prose.document_label = "Prose appendix".to_string();
    lower_ranked_prose.score = Some(10.0);
    let mut ranked_context = source_units.clone();
    ranked_context.push(lower_ranked_prose);

    let answer =
        build_ordered_source_slice_answer(&source_slice_ir(2), &source_units, &ranked_context)
            .expect("top-ranked record inventory answer");

    assert_eq!(answer.unit_count, 2);
    assert!(answer.answer.contains("first inventory row"));
    assert!(answer.answer.contains("second inventory row"));
}

#[test]
fn ordered_source_units_answer_lists_every_unit_once() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut second = source_unit(
        2,
        "[unit_id=b occurred_at=2026-01-02T00:00:00+00:00 actor_label=Assistant] second",
    );
    second.document_id = document_id;
    second.revision_id = revision_id;
    let mut first =
        source_unit(1, "[unit_id=a occurred_at=2026-01-01T00:00:00+00:00 actor_label=User] first");
    first.document_id = document_id;
    first.revision_id = revision_id;
    let answer = build_ordered_source_units_answer(&source_slice_ir(2), &[second, first])
        .expect("source slice answer");

    assert!(answer.starts_with("`records.jsonl` - 2/2"));
    assert!(answer.find("first").unwrap() < answer.find("second").unwrap());
    assert_eq!(answer.matches("\n1. ").count(), 1);
    assert_eq!(answer.matches("\n2. ").count(), 1);
}

#[test]
fn ordered_source_units_answer_reports_partial_count() {
    let answer = build_ordered_source_units_answer(&source_slice_ir(3), &[source_unit(1, "body")])
        .expect("source slice answer");

    assert!(answer.starts_with("`records.jsonl` - 1/3"));
    assert!(answer.contains("`ordinal=1`"));
}

#[test]
fn ordered_source_slice_unit_section_prefers_source_units_over_fallback_chunks() {
    let fallback = evidence_chunk(1, Some("paragraph"), "fallback paragraph");
    let selected = source_unit(7, "[unit_id=u-7] selected record");

    let section =
        render_ordered_source_slice_unit_section(&source_slice_ir(1), &[fallback, selected])
            .expect("source slice section");

    assert!(section.contains("returned_unit_count=1"));
    assert!(section.contains("[unit_id=u-7] selected record"));
    assert!(!section.contains("fallback paragraph"));
}

#[test]
fn latest_source_slice_answer_falls_back_to_ranked_context_chunks() {
    let older_id = Uuid::now_v7();
    let newer_id = Uuid::now_v7();
    let older_revision_id = Uuid::now_v7();
    let newer_revision_id = Uuid::now_v7();
    let long_body = (1..=20)
        .map(|index| format!("detail {index:02} for Sample Subject"))
        .collect::<Vec<_>>()
        .join("\n");
    let chunks = vec![
        evidence_chunk(0, Some("source_profile"), "[source_profile unit_count=3]"),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Guide appendix",
            "mentions compatibility with 9.9.9 but is not a release identity",
        ),
        release_identity_chunk(
            older_id,
            older_revision_id,
            0,
            90.0,
            "Release 1.0.1",
            "older detail",
        ),
        release_identity_chunk(newer_id, newer_revision_id, 0, 30.0, "Release 1.0.2", &long_body),
        release_identity_chunk(
            newer_id,
            newer_revision_id,
            1,
            20.0,
            "Release 1.0.2 duplicate",
            "duplicate",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_requires_typed_release_source_slice() {
    let mut ir = latest_source_slice_ir(2);
    ir.source_slice = None;
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "2".to_string(),
        kind: crate::domains::query_ir::LiteralKind::NumericCode,
    }];
    let older_id = Uuid::now_v7();
    let newer_id = Uuid::now_v7();
    let chunks = vec![
        release_identity_chunk(
            older_id,
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.1",
            "older detail",
        ),
        release_identity_chunk(
            newer_id,
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "newer detail",
        ),
    ];

    assert!(build_ordered_source_slice_answer(&ir, &[], &chunks).is_none());
}

#[test]
fn latest_source_slice_answer_rejects_query_ir_focus_body_versions() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut older = release_relevance_chunk(
        document_id,
        revision_id,
        1,
        90.0,
        "Sample Subject release history",
        "Version 1.0.1\nolder detail",
    );
    older.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
    let mut newer = release_relevance_chunk(
        document_id,
        revision_id,
        0,
        80.0,
        "Sample Subject release history",
        "Version 1.0.2\nnewer detail",
    );
    newer.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
    let noise = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Sample Subject release history",
        "Compatibility note 9.9.9\nnot a focused release unit",
    );

    let answer =
        build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &[older, noise, newer]);

    assert!(answer.is_none());
}

#[test]
fn latest_source_slice_answer_rejects_generic_source_context_body_versions() {
    let mut focused = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        80.0,
        "Sample Subject release history",
        "Version 1.0.3\ngeneric source-context detail",
    );
    focused.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
    focused.excerpt = "Version 1.0.2\nfocused excerpt detail".to_string();

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[], &[focused]);

    assert!(answer.is_none());
}

#[test]
fn latest_source_slice_answer_keeps_attested_lane_ahead_of_higher_noise_version() {
    let mut attested = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        80.0,
        "Sample Subject release history",
        "Version 3.0.0\nattested release detail",
    );
    attested.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
    let mut compatibility_noise = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Sample Subject compatibility 99.0.0",
        "compatibility-only detail",
    );
    compatibility_noise.score_kind =
        crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

    let answer = build_ordered_source_slice_answer(
        &latest_source_slice_ir(1),
        &[],
        &[compatibility_noise, attested],
    );

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_does_not_drop_attested_lane_for_a_larger_noise_family() {
    let mut attested = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        80.0,
        "Primary release history",
        "Version 3.0.0\nattested release detail",
    );
    attested.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
    let mut noise_99 = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Compatibility record 99.0.0",
        "compatibility-only detail",
    );
    noise_99.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
    let mut noise_98 = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        90.0,
        "Compatibility record 98.0.0",
        "compatibility-only detail",
    );
    noise_98.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

    let answer = build_ordered_source_slice_answer(
        &latest_source_slice_ir(2),
        &[],
        &[noise_99, noise_98, attested],
    );

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_keeps_typed_source_unit_ahead_of_a_larger_noise_family() {
    let mut typed_unit = source_unit(1, "Version 3.0.0\nprimary structured source-unit detail");
    typed_unit.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
    let mut noise_99 = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Compatibility record 99.0.0",
        "compatibility-only detail",
    );
    noise_99.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
    let mut noise_98 = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        90.0,
        "Compatibility record 98.0.0",
        "compatibility-only detail",
    );
    noise_98.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

    let answer = build_ordered_source_slice_answer(
        &latest_source_slice_ir(2),
        &[typed_unit],
        &[noise_99, noise_98],
    );

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_does_not_fill_from_unattested_source_units() {
    let mut unattested = source_unit(1, "Version 99.0.0\ncompatibility-only detail");
    unattested.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
    let mut attested = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        80.0,
        "Sample Subject release history",
        "Version 3.0.0\nattested release detail",
    );
    attested.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

    let answer =
        build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[unattested], &[attested]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_ignores_plain_relevance_body_versions() {
    let chunks = vec![release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Sample Subject release history",
        "Version 1.0.2\nplain relevance noise",
    )];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none());
}

#[test]
fn latest_source_slice_answer_ignores_document_identity_body_only_versions() {
    let chunks = vec![release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Guide appendix",
        "Version 9.9.9\ncompatibility note, not a release identity",
    )];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none());
}

#[test]
fn latest_source_slice_answer_removes_markdown_decoration() {
    let ir = latest_source_slice_ir(1);
    let document_id = Uuid::now_v7();
    let chunks = vec![release_identity_chunk(
        document_id,
        Uuid::now_v7(),
        0,
        10.0,
        "Sample Subject Release 1.0.2",
        "# Version 1.0.2\n![preview](https://example.invalid/preview.png)\n[diagram](https://example.invalid/diagram.png)\n---\n## Changes\nAdded deterministic inventory output.",
    )];

    let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_uses_typed_source_context_unit_payload() {
    let mut unit = source_unit(
        1,
        "[unit_id=u-1 occurred_at=2026-01-02T00:00:00+00:00 actor_label=Recorder] ![preview](asset.png)\n\
             Version 1.0.2\n\
             - Added neutral evidence line\n\
             - Added second neutral evidence line",
    );
    unit.document_label = "Neutral record stream".to_string();
    unit.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_orders_grounded_version_before_runtime_rank() {
    let mut ranked_unit = source_unit(4, "[unit_id=u-4 version=1.0.1 change=ranked-evidence]");
    ranked_unit.document_label = "Neutral inventory".to_string();
    ranked_unit.score_kind =
        crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
    ranked_unit.score = Some(120.0);

    let mut newer_noise = source_unit(1, "[unit_id=u-1 version=9.9.9 change=unrelated-newer]");
    newer_noise.document_label = "Neutral inventory".to_string();
    newer_noise.score_kind =
        crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
    newer_noise.score = Some(80.0);

    let answer = build_ordered_source_slice_answer(
        &latest_source_slice_ir(1),
        &[newer_noise, ranked_unit],
        &[],
    );

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_merges_newer_context_with_stale_source_units() {
    let mut stale = source_unit(9, "[unit_id=stale] Version 1.0.0\n- Stale neutral change");
    stale.document_label = "Neutral release history".to_string();
    let newer = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Version 3.0.0",
        "Version 3.0.0\n- Newest neutral change",
    );
    let middle = release_relevance_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        90.0,
        "Version 2.0.0",
        "Version 2.0.0\n- Middle neutral change",
    );

    let answer =
        build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[stale], &[middle, newer]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_dedupes_title_variants_for_same_typed_source() {
    let repeated_document_id = Uuid::now_v7();
    let repeated_revision_id = Uuid::now_v7();
    let chunks = vec![
        release_identity_chunk(
            repeated_document_id,
            repeated_revision_id,
            0,
            100.0,
            "Delta 9.0.5",
            "newest neutral change",
        ),
        release_identity_chunk(
            repeated_document_id,
            repeated_revision_id,
            0,
            99.0,
            "Delta 9.0.5 - Administration",
            "duplicate rendering of the newest neutral change",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            98.0,
            "Delta Suite 9.0.4",
            "second neutral change",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            97.0,
            "Delta Suite 9.0.3 - Delta Suite Administration",
            "third neutral change",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            96.0,
            "Delta Suite 9.0.2 - Delta Suite Administration",
            "fourth neutral change",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            95.0,
            "Delta Suite 9.0.1 - Delta Suite Administration",
            "fifth neutral change",
        ),
    ];

    let query_ir = latest_source_slice_ir(5);
    let answer = build_ordered_source_slice_answer(&query_ir, &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_keeps_generic_title_variants_from_distinct_sources() {
    let chunks = vec![
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Delta Suite 9.0.5",
            "first independent record",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            99.0,
            "Delta Suite 9.0.5 - Administration",
            "second independent record",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_keeps_exact_titles_from_distinct_sources() {
    let chunks = vec![
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Delta Suite 9.0.5",
            "first exact-title record",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            99.0,
            "Delta Suite 9.0.5",
            "second exact-title record",
        ),
    ];

    let query_ir = latest_source_slice_ir(2);
    let answer = build_ordered_source_slice_answer(&query_ir, &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_renders_body_version_instead_of_internal_ordinal() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut release = release_relevance_chunk(
        document_id,
        revision_id,
        37,
        100.0,
        "Neutral release history",
        "Version 7.8.901\n- Added a neutral capability",
    );
    release.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[], &[release]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_skips_plain_overview_version_for_release_marker() {
    let overview = release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        100.0,
        "Sample Product 5.0",
        "Sample Product 5.0 overview and compatibility notes.",
    );
    let release = release_identity_chunk(
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        50.0,
        "Build 7.8.901 - Neutral stream",
        "Changed neutral behavior.",
    );

    let answer =
        build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[], &[overview, release]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_preserves_version_header_payload() {
    let mut unit = source_unit(1, "[unit_id=u-1 version=1.0.2 change=neutral-header-evidence]");
    unit.document_label = "Neutral record stream".to_string();
    unit.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_preserves_single_document_version_label_heading() {
    let mut unit = source_unit(1, "[unit_id=u-1] Changed neutral behavior.");
    unit.document_label = "Build 7.8.901 - Neutral stream".to_string();

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[]);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_does_not_infer_dominance_from_exact_title_families() {
    let chunks = vec![
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "alpha newer",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.1",
            "alpha older",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Beta Tool Release 9.0.0",
            "beta unrelated",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_does_not_infer_dominance_from_title_extensions() {
    let chunks = vec![
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Delta Suite 3.0.3",
            "desired newest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Delta Suite 3.0.2 - Delta Suite Administration",
            "desired middle",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Delta Suite 3.0.1 - Delta Suite Administration",
            "desired oldest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Omega Stack 99.0.2",
            "noise newest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Omega Stack 99.0.1",
            "noise older",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(3), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_orders_mixed_sources_by_release_version() {
    let chunks = vec![
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Legacy Stream Release 1.0.3",
            "legacy newest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Legacy Stream Release 1.0.2",
            "legacy middle",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Legacy Stream Release 1.0.1",
            "legacy oldest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Current Stream Release 9.0.2",
            "current newest",
        ),
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Current Stream Release 9.0.1",
            "current older",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn latest_source_slice_answer_does_not_apply_title_family_filtering() {
    let mut chunks = (1..=6)
        .map(|version| {
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                &format!("Legacy Stream Release 1.0.{version}"),
                "legacy change",
            )
        })
        .collect::<Vec<_>>();
    chunks.extend((1..=4).map(|version| {
        release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            &format!("Current Stream Release 9.0.{version}"),
            "current change",
        )
    }));

    let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(5), &[], &chunks);

    assert!(answer.is_none(), "release-shaped source slices must continue to synthesis");
}

#[test]
fn low_confidence_context_release_series_does_not_infer_latest_inventory() {
    let alpha_new_id = Uuid::now_v7();
    let alpha_old_id = Uuid::now_v7();
    let beta_id = Uuid::now_v7();
    let mut ir = low_confidence_concept_ir();
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "2".to_string(),
        kind: crate::domains::query_ir::LiteralKind::NumericCode,
    }];
    let chunks = vec![
        release_relevance_chunk(
            alpha_old_id,
            Uuid::now_v7(),
            0,
            5.0,
            "Sample Subject Release 1.0.1",
            "older alpha detail",
        ),
        release_relevance_chunk(
            beta_id,
            Uuid::now_v7(),
            0,
            100.0,
            "Beta Tool Release 9.0.0",
            "beta distractor",
        ),
        release_relevance_chunk(
            alpha_new_id,
            Uuid::now_v7(),
            0,
            1.0,
            "Sample Subject Release 1.0.2",
            "newer alpha detail",
        ),
    ];

    assert!(build_ordered_source_slice_answer(&ir, &[], &chunks).is_none());
}

#[test]
fn context_release_series_fallback_skips_exact_version_questions() {
    let mut ir = low_confidence_concept_ir();
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "1.0.2".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Version,
    }];
    let chunks = vec![
        release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "exact detail",
        ),
        release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            9.0,
            "Sample Subject Release 1.0.1",
            "older detail",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

    assert!(answer.is_none());
}

#[test]
fn context_release_series_fallback_skips_high_confidence_non_release_ir() {
    let mut ir = low_confidence_concept_ir();
    ir.confidence = 0.9;
    let chunks = vec![
        release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "newer detail",
        ),
        release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            9.0,
            "Sample Subject Release 1.0.1",
            "older detail",
        ),
    ];

    let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

    assert!(answer.is_none());
}
