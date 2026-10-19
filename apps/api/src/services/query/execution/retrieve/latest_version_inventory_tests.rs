use super::test_prelude::*;

#[test]
fn untyped_query_does_not_activate_latest_version_selection_from_retrieved_evidence() {
    let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
    let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
    let alpha_old = document_row("alpha-2-2.md", "Sample Subject 2.2.0 changelog");
    let beta_new = document_row("beta-2-4.md", "Beta Suite 2.4.0 changelog");
    let index = HashMap::from([
        (alpha_new.document_id, alpha_new.clone()),
        (alpha_mid.document_id, alpha_mid.clone()),
        (alpha_old.document_id, alpha_old.clone()),
        (beta_new.document_id, beta_new.clone()),
    ]);
    let _retrieved_chunks = [
        setup_focus_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        setup_focus_runtime_chunk(alpha_new.document_id, 0, 1.0),
        setup_focus_runtime_chunk(beta_new.document_id, 0, 1.0),
    ];
    let query_ir = low_confidence_untyped_query_ir();

    let selection = select_latest_version_documents(Some(&query_ir), &index);

    assert_eq!(selection.requested_count, 0);
    assert!(selection.documents.is_empty());
}

#[test]
fn typed_latest_version_query_selects_requested_inventory_window() {
    let documents = (1..=11)
        .map(|minor| {
            document_row(
                &format!("alpha-2-{minor}.md"),
                &format!("Sample Subject 2.{minor}.0 changelog"),
            )
        })
        .collect::<Vec<_>>();
    let index = documents
        .iter()
        .cloned()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let mut query_ir = low_confidence_untyped_query_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
    query_ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
        direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        count: Some(10),
        filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
    });

    let selection = select_latest_version_documents(Some(&query_ir), &index);

    assert_eq!(selection.requested_count, 10);
    assert_eq!(selection.documents.len(), 10);
    assert_eq!(
        selection.documents.first().map(|document| document.title.as_str()),
        Some("Sample Subject 2.11.0 changelog")
    );
}

#[test]
fn typed_latest_version_query_keeps_title_variants_from_distinct_documents() {
    let titles = [
        "Delta Suite 9.0.5",
        "Delta Suite 9.0.5 - Delta Suite Administration",
        "Delta Suite 9.0.4",
        "Delta Suite 9.0.3 - Delta Suite Administration",
        "Delta Suite 9.0.2 - Delta Suite Administration",
        "Delta Suite 9.0.1 - Delta Suite Administration",
    ];
    let documents = titles
        .into_iter()
        .enumerate()
        .map(|(index, title)| document_row(&format!("delta-{index}.md"), title))
        .collect::<Vec<_>>();
    let index = documents
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let mut query_ir = low_confidence_untyped_query_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
    query_ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
        direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        count: Some(5),
        filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
    });

    let selection = select_latest_version_documents(Some(&query_ir), &index);
    let versions =
        selection.documents.iter().map(|document| document.version.clone()).collect::<Vec<_>>();

    assert_eq!(
        versions,
        vec![vec![9, 0, 5], vec![9, 0, 5], vec![9, 0, 4], vec![9, 0, 3], vec![9, 0, 2]]
    );
}

#[test]
fn typed_latest_version_query_does_not_infer_dominance_from_titles() {
    let titles = [
        "Delta Suite 3.0.3",
        "Delta Suite 3.0.2 - Delta Suite Administration",
        "Delta Suite 3.0.1 - Delta Suite Administration",
        "Omega Stack 99.0.2",
        "Omega Stack 99.0.1",
    ];
    let documents = titles
        .into_iter()
        .enumerate()
        .map(|(index, title)| document_row(&format!("family-{index}.md"), title))
        .collect::<Vec<_>>();
    let index = documents
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let selected = latest_version_documents(&index, 3, &[]);

    assert_eq!(selected.len(), 3);
    assert!(selected.iter().any(|document| document.title.starts_with("Delta Suite")));
    assert!(selected.iter().any(|document| document.title.starts_with("Omega Stack")));
    assert_eq!(selected[0].version, vec![99, 0, 2]);
    assert_eq!(selected[2].version, vec![3, 0, 3]);
}

#[test]
fn latest_version_document_dedupe_does_not_collapse_title_extensions() {
    let version = vec![7, 7, 7];
    let extended_title = "Family 0 Suite 7.7.7 - Family Administration";
    let mut rows = vec![LatestVersionDocument {
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        version: version.clone(),
        title: extended_title.to_string(),
    }];
    rows.extend((0..2_048).map(|index| {
        let title = format!("Family {index} Suite 7.7.7");
        LatestVersionDocument {
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            version: version.clone(),
            title,
        }
    }));

    let deduped = dedupe_latest_version_documents(rows);

    assert_eq!(deduped.len(), 2_049);
    assert_eq!(
        deduped.iter().filter(|document| document.title.contains("Family 0 Suite")).count(),
        2
    );
}

#[test]
fn latest_version_document_dedupe_keeps_exact_titles_from_distinct_documents() {
    let title = "Delta Suite 9.0.5";
    let rows = vec![
        LatestVersionDocument {
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            version: vec![9, 0, 5],
            title: title.to_string(),
        },
        LatestVersionDocument {
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            version: vec![9, 0, 5],
            title: title.to_string(),
        },
    ];

    let deduped = dedupe_latest_version_documents(rows);

    assert_eq!(deduped.len(), 2);
}

#[test]
fn latest_version_document_dedupe_collapses_same_document_revision() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        LatestVersionDocument {
            document_id,
            revision_id,
            version: vec![9, 0, 5],
            title: "Delta 9.0.5".to_string(),
        },
        LatestVersionDocument {
            document_id,
            revision_id,
            version: vec![9, 0, 5],
            title: "Delta 9.0.5 - Administration".to_string(),
        },
    ];

    let deduped = dedupe_latest_version_documents(rows);

    assert_eq!(deduped.len(), 1);
}
