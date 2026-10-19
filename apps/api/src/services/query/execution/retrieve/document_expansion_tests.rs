use super::test_prelude::*;

#[test]
fn artifact_sibling_source_candidates_use_image_title_prefix() {
    let image = document_row("alpha-connector-screen.png", "Alpha Connector: status screen.png");
    let manual = document_row("alpha-connector-manual.md", "Alpha Connector - operator manual");
    let unrelated = document_row("beta-connector-manual.md", "Beta Connector - operator manual");
    let document_index = [image.clone(), manual.clone(), unrelated]
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let mut image_chunk = setup_focus_runtime_chunk(image.document_id, 0, 1.0);
    image_chunk.score_kind = RuntimeChunkScoreKind::Relevance;

    let candidates = artifact_sibling_source_document_ids(&[image_chunk], &document_index, 10);

    assert_eq!(candidates.first().copied(), Some(manual.document_id));
    assert!(!candidates.contains(&image.document_id));
}

#[test]
fn artifact_sibling_source_candidates_follow_attachment_parent_page_id() {
    let image = document_row_with_source(
        "alpha-connector-screen.png",
        "Alpha Connector: status screen.png",
        Some("upload://alpha-connector-screen.png"),
        Some("https://example.invalid/download/attachments/12345/status-screen.png"),
    );
    let parent_page = document_row_with_source(
        "alpha-connector-page.html",
        "Alpha Connector",
        Some("upload://file.html"),
        Some("https://example.invalid/pages/viewpage.action?pageId=12345"),
    );
    let title_neighbor =
        document_row("alpha-connector-notes.md", "Alpha Connector - screenshot notes");
    let document_index = [image.clone(), title_neighbor, parent_page.clone()]
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let mut image_chunk = setup_focus_runtime_chunk(image.document_id, 0, 1.0);
    image_chunk.score_kind = RuntimeChunkScoreKind::Relevance;

    let candidates = artifact_sibling_source_document_ids(&[image_chunk], &document_index, 10);

    assert_eq!(candidates.first().copied(), Some(parent_page.document_id));
    assert!(!candidates.contains(&image.document_id));
}

#[test]
fn artifact_sibling_source_revision_ids_include_readable_history() {
    let document = document_row("alpha-connector-page.html", "Alpha Connector");
    let current_revision_id = document.readable_revision_id.expect("readable head");
    let older_revision_id = Uuid::now_v7();
    let processing_revision_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();
    let revisions = vec![
        revision_row(document.document_id, current_revision_id, 4, "ready"),
        revision_row(document.document_id, processing_revision_id, 3, "processing"),
        revision_row(other_document_id, other_revision_id, 2, "ready"),
        revision_row(document.document_id, older_revision_id, 1, "text_readable"),
    ];

    let revision_ids =
        artifact_sibling_source_revision_ids(&document, current_revision_id, &revisions, 8);

    assert_eq!(revision_ids, vec![current_revision_id, older_revision_id]);
}

#[test]
fn artifact_sibling_source_merge_keeps_source_context_kind() {
    let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 0.9);
    primary_chunk.score_kind = RuntimeChunkScoreKind::Relevance;
    let mut sibling_chunk =
        setup_focus_runtime_chunk(Uuid::now_v7(), 0, artifact_sibling_source_chunk_score(0, 0));
    sibling_chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
    let sibling_chunk_id = sibling_chunk.chunk_id;

    let merged = merge_query_ir_focus_chunks(vec![primary_chunk], vec![sibling_chunk], 8);
    let merged_sibling = merged
        .iter()
        .find(|chunk| chunk.chunk_id == sibling_chunk_id)
        .expect("sibling source context chunk retained");

    assert_eq!(merged_sibling.score_kind, RuntimeChunkScoreKind::SourceContext);
    assert!(score_value(merged_sibling.score) < DOCUMENT_IDENTITY_SCORE_FLOOR);
}

#[test]
fn query_ir_focus_merge_keeps_focused_document_kind() {
    let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 0.9);
    primary_chunk.score_kind = RuntimeChunkScoreKind::Relevance;
    let mut focused_document =
        setup_focus_runtime_chunk(Uuid::now_v7(), 0, document_identity_chunk_score(0, 0));
    focused_document.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    let focused_document_chunk_id = focused_document.chunk_id;

    let merged = merge_query_ir_focus_chunks(vec![primary_chunk], vec![focused_document], 8);
    let merged_focused_document = merged
        .iter()
        .find(|chunk| chunk.chunk_id == focused_document_chunk_id)
        .expect("focused document chunk retained");

    assert_eq!(merged_focused_document.score_kind, RuntimeChunkScoreKind::FocusedDocument);
}

#[test]
fn query_ir_focus_merge_reserves_source_context_against_document_identity_hits() {
    let primary_chunks = (0..16)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                Uuid::now_v7(),
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut source_context =
        setup_focus_runtime_chunk(Uuid::now_v7(), 20, ARTIFACT_SIBLING_SOURCE_SCORE_BASE);
    source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
    source_context.source_text = "companion source evidence".to_string();
    let source_context_chunk_id = source_context.chunk_id;

    let merged = merge_query_ir_focus_chunks(primary_chunks, vec![source_context], 8);

    assert!(merged.iter().any(|chunk| chunk.chunk_id == source_context_chunk_id), "{merged:#?}");
}

#[test]
fn query_ir_focus_merge_reserves_focused_document_against_document_identity_hits() {
    let primary_chunks = (0..16)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                Uuid::now_v7(),
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused_document =
        setup_focus_runtime_chunk(Uuid::now_v7(), 20, document_identity_chunk_score(0, 0));
    focused_document.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    let focused_document_chunk_id = focused_document.chunk_id;

    let merged = merge_query_ir_focus_chunks(primary_chunks, vec![focused_document], 8);

    assert!(merged.iter().any(|chunk| chunk.chunk_id == focused_document_chunk_id), "{merged:#?}");
}

#[test]
fn truncate_reserves_comparison_literal_context_against_identity_hits() {
    let target_document_id = Uuid::now_v7();
    let mut query_ir = setup_query_ir("Alpha Beta");
    query_ir.act = QueryAct::Compare;
    query_ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
        a: Some("Alpha".to_string()),
        b: Some("Beta".to_string()),
        dimension: "threshold control".to_string(),
    });
    query_ir.retrieval_query =
        Some("Alpha Beta threshold control SAMPLE_LIMIT configuration".to_string());

    let mut chunks = (0..12)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                Uuid::now_v7(),
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk.document_label = format!("General comparison reference {index}");
            chunk.source_text =
                "Alpha and Beta both describe threshold control behavior in prose.".to_string();
            chunk
        })
        .collect::<Vec<_>>();
    let mut exact = setup_focus_runtime_chunk(target_document_id, 20, 1.0);
    exact.score_kind = RuntimeChunkScoreKind::Relevance;
    exact.document_label = "Alpha Beta comparison".to_string();
    exact.source_text = concat!(
        "AlphaLimitRequests: read SAMPLE_LIMIT_REQUESTS=100\n",
        "AlphaLimitWindowSeconds: read SAMPLE_LIMIT_WINDOW_SECONDS=60"
    )
    .to_string();
    let exact_id = exact.chunk_id;
    chunks.push(exact);

    truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

    assert!(chunks.iter().any(|chunk| chunk.chunk_id == exact_id), "{chunks:#?}");
    assert_eq!(chunks.len(), 8);
}

#[test]
fn query_ir_focus_merge_reserves_latest_version_inventory() {
    let release_document_id = Uuid::now_v7();
    let latest_chunks = (0..4)
        .map(|index| {
            latest_version_runtime_chunk(
                release_document_id,
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 10.0 - index as f32,
            )
        })
        .collect::<Vec<_>>();
    let latest_chunk_ids =
        latest_chunks.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    let focus_chunks = (0..16)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                Uuid::now_v7(),
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 500.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
            chunk
        })
        .collect::<Vec<_>>();

    let merged = merge_query_ir_focus_chunks(latest_chunks, focus_chunks, 5);

    assert_eq!(merged.len(), 5);
    for chunk_id in latest_chunk_ids {
        assert!(merged.iter().any(|chunk| chunk.chunk_id == chunk_id), "{merged:#?}");
    }
}

#[test]
fn latest_version_scoped_document_ids_include_semantic_chunks() {
    let structural_document_id = Uuid::now_v7();
    let semantic_document_id = Uuid::now_v7();
    let structural_document = LatestVersionDocument {
        document_id: structural_document_id,
        revision_id: Uuid::now_v7(),
        version: vec![1, 0, 2],
        title: "Sample Subject release 1.0.2".to_string(),
    };
    let mut semantic_chunk =
        latest_version_runtime_chunk(semantic_document_id, 0, DOCUMENT_IDENTITY_SCORE_FLOOR);
    semantic_chunk.document_label = "Sample Subject changelog".to_string();

    let ids = latest_version_scoped_document_ids(&[structural_document], &[semantic_chunk]);

    assert!(ids.contains(&structural_document_id));
    assert!(ids.contains(&semantic_document_id));
}

#[test]
fn document_evidence_anchor_candidates_match_document_identity_terms() {
    let manifest = document_row("sample_manifest.yaml", "Sample manifest");
    let orchestration = document_row("sample_orchestration.md", "Sample orchestration reference");
    let infra = document_row("sample_infra.plan", "Sample infrastructure reference");
    let document_index = HashMap::from([
        (manifest.document_id, manifest.clone()),
        (orchestration.document_id, orchestration),
        (infra.document_id, infra),
    ]);
    let mut query_ir = setup_query_ir("Sample Manifest");
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Service];
    query_ir.document_focus = None;
    query_ir.target_entities.clear();
    query_ir.retrieval_query = Some("alpha entries fields values".to_string());

    let candidates = document_evidence_anchor_candidate_document_ids(
        "What services and ports are defined in the Sample Manifest configuration?",
        Some(&query_ir),
        &document_index,
        3,
    );

    assert_eq!(candidates.first(), Some(&manifest.document_id));
}

#[test]
fn artifact_sibling_source_revision_ids_match_the_retention_window() {
    let document = document_row("alpha-connector-page.html", "Alpha Connector");
    let current_revision_id = document.readable_revision_id.expect("readable head");
    let history_revision_ids = (0..9).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let revisions = std::iter::once((current_revision_id, 10_i64))
        .chain(
            history_revision_ids
                .iter()
                .enumerate()
                .map(|(index, revision_id)| (*revision_id, 9 - index as i64)),
        )
        .map(|(revision_id, revision_number)| {
            revision_row(document.document_id, revision_id, revision_number, "text_readable")
        })
        .collect::<Vec<_>>();
    let candidates = revisions
        .iter()
        .map(|revision| crate::domains::content::HistoricalProjectionRevision {
            revision_id: revision.revision_id,
            document_id: revision.document_id,
            text_state: revision.text_state.as_str(),
        })
        .collect::<Vec<_>>();
    let expected = crate::domains::content::historical_projection_retained_revision_ids(
        document.document_id,
        current_revision_id,
        &candidates,
        crate::domains::content::HISTORICAL_PROJECTION_RETENTION_REVISIONS,
    );

    let revision_ids = artifact_sibling_source_revision_ids(
        &document,
        current_revision_id,
        &revisions,
        crate::domains::content::HISTORICAL_PROJECTION_RETENTION_REVISIONS,
    );

    assert_eq!(revision_ids, expected);
    assert_eq!(revision_ids.len(), 8);
    assert_eq!(revision_ids.first().copied(), Some(current_revision_id));
}
