use super::test_prelude::*;

#[test]
fn latest_version_question_detection_uses_query_ir() {
    assert!(!query_requests_latest_versions(&release_query_ir(Some("5"), None)));
    assert!(query_requests_latest_versions(&release_query_ir_with_source_slice_count(Some(5))));
    let mut release_target_ir = release_query_ir_with_source_slice_count(Some(10));
    release_target_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Release];
    assert!(query_requests_latest_versions(&release_target_ir));
    assert!(!query_requests_latest_versions(&release_query_ir_with_source_slice(
        SourceSliceDirection::Head,
        Some(5)
    )));
    assert!(!query_requests_latest_versions(&release_query_ir_with_source_slice(
        SourceSliceDirection::All,
        Some(5)
    )));
    let mut exact_version_ir = release_query_ir(None, None);
    exact_version_ir.literal_constraints =
        vec![LiteralSpan { text: "1.2.3".to_string(), kind: LiteralKind::Version }];
    assert!(!query_requests_latest_versions(&exact_version_ir));
    exact_version_ir.source_slice = Some(SourceSliceSpec {
        direction: SourceSliceDirection::Tail,
        count: Some(3),
        filter: SourceSliceFilter::ReleaseMarker,
    });
    assert!(query_requests_latest_versions(&exact_version_ir));
    let mut ir = release_query_ir(None, None);
    ir.target_types.clear();
    assert!(!query_requests_latest_versions(&ir));
}

#[test]
fn requested_latest_version_count_defaults_and_caps() {
    assert_eq!(requested_latest_version_count(&release_query_ir(None, None)), 10);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("3"), None)), 3);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("100"), None)), 20);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("2024"), None)), 10);
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(10))),
        10
    );
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(42))),
        20
    );
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(0))),
        1
    );
    assert_eq!(requested_latest_version_count(&release_query_ir_with_source_slice_count(None)), 10);

    let mut source_slice_ir = release_query_ir(Some("3"), None);
    source_slice_ir.source_slice = Some(SourceSliceSpec {
        direction: SourceSliceDirection::Tail,
        count: Some(8),
        filter: SourceSliceFilter::ReleaseMarker,
    });
    assert_eq!(requested_latest_version_count(&source_slice_ir), 8);
}

#[test]
fn latest_version_scope_terms_keep_digit_bearing_subject_tokens() {
    let mut ir = release_query_ir(Some("10"), Some("Alpha2 Pay"));
    ir.document_focus = Some(DocumentHint { hint: "Release notes for Suite4".to_string() });
    ir.literal_constraints
        .push(LiteralSpan { text: "2024".to_string(), kind: LiteralKind::NumericCode });

    let terms = latest_version_scope_terms(&ir);

    assert!(terms.contains(&"alpha2".to_string()));
    assert!(terms.contains(&"suite4".to_string()));
    assert!(!terms.contains(&"10".to_string()));
    assert!(!terms.contains(&"2024".to_string()));
}

#[test]
fn latest_version_chunk_merge_limit_preserves_requested_document_coverage() {
    assert_eq!(latest_version_context_top_k(&release_query_ir(Some("10"), None), 8), 8);
    assert_eq!(
        latest_version_context_top_k(&release_query_ir_with_source_slice_count(Some(10)), 8),
        40
    );
    assert_eq!(
        latest_version_context_top_k(
            &release_query_ir_with_source_slice(SourceSliceDirection::Head, Some(10)),
            8
        ),
        8
    );
    assert_eq!(latest_version_context_top_k(&release_query_ir(Some("3"), None), 20), 20);
}

#[test]
fn latest_version_chunk_score_keeps_first_chunk_for_each_version_before_second_chunks() {
    let newest_second = latest_version_chunk_score(DOCUMENT_IDENTITY_SCORE_FLOOR, 5, 0, 1);
    let oldest_first = latest_version_chunk_score(DOCUMENT_IDENTITY_SCORE_FLOOR, 5, 4, 0);

    assert!(oldest_first > newest_second);
}

#[test]
fn extract_semver_like_version_reads_title_versions() {
    assert_eq!(extract_semver_like_version("Version 9.8.765 - Product"), Some(vec![9, 8, 765]));
    assert_eq!(extract_semver_like_version("Version 12.402 - Product"), Some(vec![12, 402]));
    assert_eq!(extract_semver_like_version("No release number"), None);
    assert_eq!(extract_semver_like_version("Screenshot 2026.05.10"), None);
    assert_eq!(extract_semver_like_version("Screenshot 2026.05"), None);
}

#[test]
fn compare_version_desc_orders_newer_versions_first() {
    assert_eq!(compare_version_desc(&[9, 8, 765], &[9, 8, 764]), std::cmp::Ordering::Less);
    assert_eq!(compare_version_desc(&[9, 8, 762], &[9, 8, 763]), std::cmp::Ordering::Greater);
}

#[test]
fn latest_version_documents_select_newest_distinct_versions() {
    let docs = [
        sample_document_row("release-9.8.762.html", "Version 9.8.762"),
        sample_document_row("release-9.8.765.html", "Version 9.8.765"),
        sample_document_row("release-9.8.763.html", "Version 9.8.763"),
        sample_document_row("guide.html", "Setup Guide"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);
    let versions = selected.into_iter().map(|document| document.version).collect::<Vec<_>>();

    assert_eq!(versions, vec![vec![9, 8, 765], vec![9, 8, 763], vec![9, 8, 762]]);
}

#[test]
fn latest_version_documents_require_a_readable_revision() {
    let mut preparing_only = sample_document_row("release-10.0.0.html", "Version 10.0.0");
    preparing_only.readable_revision_id = None;
    let prepared = sample_document_row("release-9.0.0.html", "Version 9.0.0");
    let prepared_id = prepared.document_id;
    let index = [preparing_only, prepared]
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 2, &[]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].document_id, prepared_id);
}

#[test]
fn latest_version_density_sql_uses_only_exact_current_readable_prepared_chunks() {
    let sql = super::latest_version_selection::LATEST_VERSION_STRUCTURAL_DENSITY_CANDIDATES_SQL;

    for predicate in [
        "unnest($2::uuid[], $6::uuid[])",
        "d.readable_revision_id = candidate.revision_id",
        "sr.revision_id = candidate.revision_id",
        "sr.document_id = d.document_id",
        "sr.workspace_id = d.workspace_id",
        "sr.library_id = d.library_id",
        "sr.preparation_state = 'prepared'",
        "candidate.document_id = c.document_id",
        "candidate.workspace_id = c.workspace_id",
        "candidate.library_id = c.library_id",
        "candidate.revision_id = c.revision_id",
        "c.chunk_state = 'ready'",
    ] {
        assert!(sql.contains(predicate), "missing exact-scope predicate: {predicate}");
    }
    assert!(sql.contains("d.document_state = 'active'"));
    assert!(sql.contains("group by document_id, revision_id"));
    assert!(sql.contains("select document_id, revision_id, version_row_count"));
    assert!(sql.contains("d.deleted_at is null"));
    assert!(!sql.contains("preparation_state = 'preparing'"));
}

#[test]
fn latest_version_documents_rank_compound_release_titles_by_newest_direct_version() {
    let docs = [
        sample_document_row("release-12.406.html", "Version 7.8 | 12.406"),
        sample_document_row("release-12.407.html", "Version 7.8 | 12.407"),
        sample_document_row("release-12.402.html", "Version 12.402"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 2, &[]);
    let versions = selected.into_iter().map(|document| document.version).collect::<Vec<_>>();

    assert_eq!(versions, vec![vec![12, 407], vec![12, 406]]);
}

#[test]
fn latest_version_documents_reject_formal_directional_version_transition_guides() {
    let docs = [
        sample_document_row("database-migration.html", "Sample Database 9.5 -> 15.3"),
        sample_document_row("platform-update.html", "Sample Platform 8.4 => 12.1"),
        sample_document_row("service-migration.html", "Sample Service migration 3.2 → 4.0"),
        sample_document_row("release-12.407.html", "Version 7.8 | 12.407"),
        sample_document_row("release-12.406.html", "Release 12.406"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 10, &[]);
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(titles, vec!["Version 7.8 | 12.407".to_string(), "Release 12.406".to_string()]);
}

#[test]
fn latest_version_documents_require_release_marker_and_respect_scope_terms() {
    let docs = [
        sample_document_row("alpha-release-9.8.765.html", "Alpha Version 9.8.765"),
        sample_document_row("beta-release-9.9.999.html", "Beta Version 9.9.999"),
        sample_document_row("oauth-2.0-guide.html", "OAuth 2.0 Guide"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(
        &index,
        5,
        &latest_version_scope_terms(&release_query_ir(None, Some("Alpha"))),
    );
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(titles, vec!["Alpha Version 9.8.765".to_string()]);
    assert!(!text_has_release_version_marker("OAuth Guide"));
}

#[test]
fn latest_version_documents_exclude_attached_context_from_inventory_slots() {
    let primary_new = sample_document_row("sample-5.md", "Sample Version 5.0");
    let primary_mid_1 = sample_document_row("sample-4.md", "Sample Version 4.0");
    let primary_mid_2 = sample_document_row("sample-3.md", "Sample Version 3.0");
    let primary_old = sample_document_row("sample-2.md", "Sample Version 2.0");
    let mut attached =
        sample_document_row("sample-5-notes.md", "Sample Version 5.0: attached notes");
    attached.document_role = crate::domains::content::DOCUMENT_ROLE_ATTACHED_CONTEXT.to_string();
    attached.parent_document_id = Some(primary_new.document_id);
    let unrelated = sample_document_row("other-1.md", "Other Version 1.0");
    let document_index =
        [primary_new, primary_mid_1, primary_mid_2, primary_old, attached.clone(), unrelated]
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&document_index, 5, &["sample".to_string()]);

    assert_eq!(selected.len(), 4, "{selected:#?}");
    assert!(
        selected.iter().all(|document| document.document_id != attached.document_id),
        "{selected:#?}"
    );
}

#[test]
fn latest_version_documents_exclude_primary_image_source_uri_with_query_and_fragment() {
    let mut primary_image = sample_document_row("sample-5.md", "Sample Version 5.0");
    primary_image.source_uri =
        Some("https://example.invalid/releases/sample-5.PNG?download=1#preview".to_string());
    let primary_document = sample_document_row("sample-4.md", "Sample Version 4.0");
    let image_document_id = primary_image.document_id;
    let document_index = [primary_image, primary_document]
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&document_index, 5, &["sample".to_string()]);

    assert_eq!(selected.len(), 1, "{selected:#?}");
    assert!(
        selected.iter().all(|document| document.document_id != image_document_id),
        "{selected:#?}"
    );
    assert_eq!(selected[0].version, vec![4, 0]);
}

#[test]
fn latest_version_documents_fall_back_when_instruction_words_are_not_scope() {
    let docs = [
        sample_document_row("release-9.8.765.html", "Version 9.8.765"),
        sample_document_row("release-9.9.999.html", "Version 9.9.999"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(
        &index,
        1,
        &latest_version_scope_terms(&release_query_ir(None, None)),
    );

    assert_eq!(selected[0].version, vec![9, 9, 999]);
}

#[test]
fn latest_version_documents_do_not_collapse_same_version_across_titles() {
    let docs = [
        sample_document_row("alpha-release-9.8.765.html", "Alpha Version 9.8.765"),
        sample_document_row("beta-release-9.8.765.html", "Beta Version 9.8.765"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 5, &[]);

    assert_eq!(selected.len(), 2);
}

#[test]
fn latest_version_documents_keep_exact_titles_from_distinct_documents() {
    let docs = [
        sample_document_row("alpha-1.2.12-a.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.12-b.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.11.html", "Version 1.2.11 - Sample Subject"),
        sample_document_row("beta-9.9.999.html", "Version 9.9.999 - Beta Suite"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);

    assert_eq!(selected.len(), 3);
    assert_eq!(
        selected.into_iter().map(|document| document.title).collect::<Vec<_>>(),
        vec![
            "Version 9.9.999 - Beta Suite".to_string(),
            "Version 1.2.12 - Sample Subject".to_string(),
            "Version 1.2.12 - Sample Subject".to_string(),
        ]
    );
}

#[test]
fn latest_version_documents_do_not_infer_dominance_from_titles() {
    let docs = [
        sample_document_row("alpha-1.2.12.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.11.html", "Version 1.2.11 - Sample Subject"),
        sample_document_row("alpha-1.2.10.html", "Version 1.2.10 - Sample Subject"),
        sample_document_row("beta-9.9.999.html", "Version 9.9.999 - Beta Suite"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(
        titles,
        vec![
            "Version 9.9.999 - Beta Suite".to_string(),
            "Version 1.2.12 - Sample Subject".to_string(),
            "Version 1.2.11 - Sample Subject".to_string(),
        ]
    );
}

#[test]
fn latest_version_documents_rank_compound_versions_without_title_family_inference() {
    let docs = [
        sample_document_row("current-12.407.html", "Version 7.8 | 12.407"),
        sample_document_row("current-12.406.html", "Version 7.8 | 12.406"),
        sample_document_row("current-12.404.html", "Version 7.8 | 12.404"),
        sample_document_row("current-12.403.html", "Version 7.8 | 12.403"),
        sample_document_row("bridge-12.402.html", "Version 12.402"),
        sample_document_row("legacy-7.8.702.html", "Version 7.8.702"),
        sample_document_row("legacy-7.8.701.html", "Version 7.8.701"),
        sample_document_row("legacy-7.8.700.html", "Version 7.8.700"),
        sample_document_row("legacy-7.8.699.html", "Version 7.8.699"),
        sample_document_row("legacy-7.8.698.html", "Version 7.8.698"),
        sample_document_row("legacy-7.8.697.html", "Version 7.8.697"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 5, &[]);
    let versions = selected.into_iter().map(|document| document.version).collect::<Vec<_>>();

    assert_eq!(
        versions,
        vec![vec![12, 407], vec![12, 406], vec![12, 404], vec![12, 403], vec![12, 402],],
    );
}

#[test]
fn map_chunk_hit_drops_orphan_documents_without_heads() {
    let mut document = sample_document_row("orphan-doc.csv", "orphan-doc.csv");
    document.active_revision_id = None;
    document.readable_revision_id = None;
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let chunk = knowledge_chunk_row(&document, 0, Some("table_row"), "stale record");

    assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
}

#[test]
fn map_chunk_hit_drops_active_revision_when_another_revision_is_readable() {
    let mut document = sample_document_row("revisioned-source.md", "Revisioned source");
    let unreadable_active_revision_id = Uuid::now_v7();
    let readable_revision_id = Uuid::now_v7();
    document.active_revision_id = Some(unreadable_active_revision_id);
    document.readable_revision_id = Some(readable_revision_id);
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let mut chunk = knowledge_chunk_row(&document, 0, Some("summary"), "unreadable summary");
    chunk.revision_id = unreadable_active_revision_id;

    assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
}

#[test]
fn map_chunk_hit_preserves_structured_literals_from_window_and_content() {
    let document = sample_document_row("sample-guide.md", "Sample Guide");
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let mut chunk = knowledge_chunk_row(
        &document,
        3,
        Some("paragraph"),
        "[Main]\nalphaMode = strict\nconfiguration path: /etc/sample/service.conf",
    );
    chunk.window_text = Some(
        "Run the reload command after editing the configuration:\n\
         samplectl reload alpha-plugin --config /etc/sample/service.conf"
            .to_string(),
    );

    let mapped = map_chunk_hit(chunk, 1.0, &document_index, &[]).unwrap();

    assert!(mapped.source_text.contains("[Main]"));
    assert!(mapped.source_text.contains("alphaMode = strict"));
    assert!(mapped.source_text.contains("samplectl reload alpha-plugin"));
    assert!(mapped.excerpt.contains("[Main]"));
    assert!(mapped.excerpt.contains("alphaMode = strict"));
    assert!(mapped.excerpt.contains("samplectl reload alpha-plugin"));
}

#[test]
fn retain_current_document_head_chunks_keeps_only_exact_readable_revisions() {
    let mut document = sample_document_row("records.jsonl", "records.jsonl");
    let unreadable_active_revision_id = Uuid::now_v7();
    let readable_revision_id = Uuid::now_v7();
    document.active_revision_id = Some(unreadable_active_revision_id);
    document.readable_revision_id = Some(readable_revision_id);
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let orphan_document_id = Uuid::now_v7();
    let mut unreadable_chunk =
        runtime_chunk_for_document(document.document_id, "unreadable revision", 1.0);
    unreadable_chunk.revision_id = unreadable_active_revision_id;
    let mut readable_chunk = runtime_chunk_for_document(document.document_id, "current", 0.9);
    readable_chunk.revision_id = readable_revision_id;
    let orphan_chunk = runtime_chunk_for_document(orphan_document_id, "orphan", 0.5);
    let mut chunks = vec![unreadable_chunk, readable_chunk, orphan_chunk];

    assert_eq!(retain_current_document_head_chunks(&mut chunks, &document_index), 2);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].document_id, document.document_id);
    assert_eq!(chunks[0].revision_id, readable_revision_id);
}
