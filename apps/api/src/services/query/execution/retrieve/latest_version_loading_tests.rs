use super::test_prelude::*;

#[test]
fn postgres_document_index_row_maps_current_heads() {
    let document_id = Uuid::now_v7();
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let active_revision_id = Uuid::now_v7();
    let readable_revision_id = Uuid::now_v7();
    let created_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
    let updated_at = Utc.with_ymd_and_hms(2026, 1, 3, 3, 4, 5).unwrap();

    let row = query_document_index_row_to_knowledge_document_row(QueryDocumentIndexRow {
        document_id,
        workspace_id,
        library_id,
        external_key: "event-stream.jsonl".to_string(),
        title: Some("Event Stream".to_string()),
        source_uri: Some("https://example.invalid/events".to_string()),
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(active_revision_id),
        readable_revision_id: Some(readable_revision_id),
        latest_revision_no: Some(4),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at,
        updated_at,
        deleted_at: None,
    });

    assert_eq!(row.document_id, document_id);
    assert_eq!(row.workspace_id, workspace_id);
    assert_eq!(row.library_id, library_id);
    assert_eq!(row.external_key, "event-stream.jsonl");
    assert_eq!(row.file_name.as_deref(), Some("event-stream.jsonl"));
    assert_eq!(row.title.as_deref(), Some("Event Stream"));
    assert_eq!(row.source_uri.as_deref(), Some("https://example.invalid/events"));
    assert_eq!(row.document_hint.as_deref(), None);
    assert_eq!(current_document_revision_id(&row), Some(readable_revision_id));
    assert_eq!(row.active_revision_id, Some(active_revision_id));
    assert_eq!(row.latest_revision_no, Some(4));
    assert_eq!(row.updated_at, updated_at);
}

#[test]
fn semantic_latest_candidates_use_structural_identity_or_scope_signal() {
    let versioned = document_row("alpha-9.8.7.html", "Alpha 9.8.7");
    let reference = document_row("alpha-reference.html", "Alpha reference");
    let image_versioned = document_row("alpha-9.8.7.svg", "Alpha 9.8.7 diagram");
    let versioned_id = versioned.document_id;
    let document_index = HashMap::from([
        (versioned.document_id, versioned),
        (reference.document_id, reference),
        (image_versioned.document_id, image_versioned),
    ]);

    let selected = latest_version_semantic_candidate_documents(&document_index, &[], 8);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].document_id, versioned_id);

    let scoped =
        latest_version_semantic_candidate_documents(&document_index, &["alpha".to_string()], 8);
    assert!(scoped.iter().any(|document| document.document_id == versioned_id));
}

#[test]
fn semantic_latest_candidates_keep_large_scoped_sets() {
    let documents = (0..40)
        .map(|index| document_row(&format!("alpha-module-{index}.html"), "Alpha module"))
        .collect::<Vec<_>>();
    let document_index =
        documents.iter().cloned().map(|document| (document.document_id, document)).collect();

    let selected = latest_version_semantic_candidate_documents(
        &document_index,
        &["alpha".to_string()],
        LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    );

    assert_eq!(selected.len(), documents.len());
}

#[test]
fn semantic_latest_structural_probe_uses_version_density_not_identity_words() {
    let dense = document_row("neutral-ledger.html", "Neutral Ledger");
    let dense_id = dense.document_id;
    let plain = document_row("neutral-manual.html", "Neutral Manual");
    let document_index =
        HashMap::from([(dense.document_id, dense.clone()), (plain.document_id, plain)]);

    assert_eq!(
        latest_version_identity_structural_score(
            &latest_version_document_identity_text(&dense),
            &[]
        ),
        0
    );

    let candidates = latest_version_structural_probe_candidate_documents(
        &document_index,
        &[],
        &BTreeSet::new(),
        8,
    );
    assert!(candidates.iter().any(|candidate| candidate.document_id == dense_id));

    let source = concat!(
        "1. 9.8.7 | token-a = enabled\n",
        "2. 9.8.6 | token-b = disabled\n",
        "3. 9.8.5 | /opt/neutral/config"
    );
    let versions = latest_version_context_versions(source);
    assert!(latest_version_structural_inventory_candidate_has_density(
        versions.len(),
        1,
        latest_version_structural_inventory_text_score(source),
    ));
}

#[test]
fn semantic_latest_unscoped_tail_prefers_dense_source_local_inventory() {
    let dense = document_row("neutral-ledger.html", "Neutral Ledger");
    let sparse = document_row("neutral-release-99.html", "Neutral Release 99.0.0");
    let dense_revision_id = current_document_revision_id(&dense).unwrap();
    let sparse_revision_id = current_document_revision_id(&sparse).unwrap();
    let mut rows = (0..5)
        .map(|offset| LatestVersionSemanticRow {
            version: vec![1, 0, offset],
            document_rank: 20,
            structural_density_score: 80,
            from_structural_inventory: true,
            row: chunk_row(
                dense.workspace_id,
                dense.library_id,
                dense.document_id,
                dense_revision_id,
                100 + offset as i32,
                &format!("{} | neutral row", 1 + offset),
            ),
        })
        .collect::<Vec<_>>();
    rows.push(LatestVersionSemanticRow {
        version: vec![99, 0, 0],
        document_rank: 1,
        structural_density_score: 4,
        from_structural_inventory: true,
        row: chunk_row(
            sparse.workspace_id,
            sparse.library_id,
            sparse.document_id,
            sparse_revision_id,
            1,
            "99.0.0 | sparse row",
        ),
    });
    for index in 0..40 {
        let document = document_row(
            &format!("neutral-sparse-{index}.html"),
            &format!("Neutral sparse {index}"),
        );
        let revision_id = current_document_revision_id(&document).unwrap();
        rows.push(LatestVersionSemanticRow {
            version: vec![50, index],
            document_rank: 30 + index as usize,
            structural_density_score: 4,
            from_structural_inventory: true,
            row: chunk_row(
                document.workspace_id,
                document.library_id,
                document.document_id,
                revision_id,
                1,
                &format!("50.{index} | sparse row"),
            ),
        });
    }

    let unscoped_tail = order_latest_version_semantic_rows(rows.clone(), true, 10);

    assert_eq!(unscoped_tail[0].row.document_id, dense.document_id);
    assert_eq!(unscoped_tail[0].row.chunk_index, 104);
    assert!(
        unscoped_tail
            .iter()
            .take(LATEST_VERSION_SEMANTIC_CHUNK_CAP)
            .filter(|row| row.row.document_id == dense.document_id)
            .count()
            >= 3,
        "{:?}",
        unscoped_tail
            .iter()
            .take(LATEST_VERSION_SEMANTIC_CHUNK_CAP)
            .map(|row| (row.row.document_id, row.row.chunk_index))
            .collect::<Vec<_>>()
    );

    let scoped_or_version_sorted = order_latest_version_semantic_rows(rows, false, 10);

    assert_eq!(scoped_or_version_sorted[0].row.document_id, sparse.document_id);
}

#[test]
fn semantic_latest_unscoped_tail_orders_each_document_by_version_not_chunk_index() {
    let history = document_row("neutral-history.html", "Neutral release history");
    let revision_id = current_document_revision_id(&history).unwrap();
    let rows = vec![
        LatestVersionSemanticRow {
            version: vec![1, 0, 1],
            document_rank: 1,
            structural_density_score: 80,
            from_structural_inventory: true,
            row: chunk_row(
                history.workspace_id,
                history.library_id,
                history.document_id,
                revision_id,
                100,
                "Version 1.0.1 | older change",
            ),
        },
        LatestVersionSemanticRow {
            version: vec![1, 0, 9],
            document_rank: 1,
            structural_density_score: 80,
            from_structural_inventory: true,
            row: chunk_row(
                history.workspace_id,
                history.library_id,
                history.document_id,
                revision_id,
                10,
                "Version 1.0.9 | newer change",
            ),
        },
    ];

    let ordered = order_latest_version_semantic_rows(rows, true, 2);

    assert_eq!(ordered[0].version, vec![1, 0, 9]);
    assert_eq!(ordered[1].version, vec![1, 0, 1]);
}

#[test]
fn semantic_latest_unscoped_tail_does_not_starve_dense_inventory_with_identity_prefix() {
    let mut rows = Vec::new();
    for index in 0..24 {
        let document = document_row(
            &format!("neutral-identity-{index}.html"),
            &format!("Neutral identity {index}.0.0"),
        );
        let revision_id = current_document_revision_id(&document).unwrap();
        rows.push(LatestVersionSemanticRow {
            version: vec![90, index],
            document_rank: index as usize,
            structural_density_score: 0,
            from_structural_inventory: false,
            row: chunk_row(
                document.workspace_id,
                document.library_id,
                document.document_id,
                revision_id,
                1,
                &format!("90.{index} | identity row"),
            ),
        });
    }

    let mut required_dense_document_id = Uuid::nil();
    for index in 0..24 {
        let document =
            document_row(&format!("neutral-dense-{index}.html"), &format!("Neutral dense {index}"));
        if index == 15 {
            required_dense_document_id = document.document_id;
        }
        let revision_id = current_document_revision_id(&document).unwrap();
        rows.push(LatestVersionSemanticRow {
            version: vec![10, index],
            document_rank: 100 + index as usize,
            structural_density_score: 100usize.saturating_sub(index as usize),
            from_structural_inventory: true,
            row: chunk_row(
                document.workspace_id,
                document.library_id,
                document.document_id,
                revision_id,
                50 + index as i32,
                &format!("10.{index} | dense row"),
            ),
        });
    }

    let ordered = order_latest_version_semantic_rows(rows, true, 10);
    let selected = ordered.iter().take(LATEST_VERSION_SEMANTIC_CHUNK_CAP).collect::<Vec<_>>();

    assert!(selected[0].from_structural_inventory);
    assert!(
        selected.iter().any(|row| row.row.document_id == required_dense_document_id),
        "{:?}",
        selected
            .iter()
            .map(|row| (row.from_structural_inventory, row.structural_density_score))
            .collect::<Vec<_>>()
    );
}
