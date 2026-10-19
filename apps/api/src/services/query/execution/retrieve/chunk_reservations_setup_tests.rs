use super::test_prelude::*;

#[test]
fn setup_focus_selection_reserves_package_path_anchor_before_parameter_rows() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut rows = Vec::new();
    rows.push(chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        1,
        "Module configuration\n\
         sample-runner --install sample-unit\n\
         sample-configure alpha-subject\n\
         Settings are stored in /opt/alpha/alpha-subject.conf in section [AlphaSubject].",
    ));
    for index in 2..8 {
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            index,
            "endpointUrl = http://localhost\n\
             timeoutSeconds = 60\n\
             primaryKey = \"\"\n\
             credentialToken = \"\"\n\
             retryInterval = 10\n\
             currency = \"USD\"",
        ));
    }

    let selected = select_setup_focus_document_rows(&rows);

    assert!(
        selected.iter().take(SETUP_VARIANT_CHUNKS_PER_DOCUMENT).any(|row| row.chunk_index == 1),
        "{selected:#?}"
    );
}

#[test]
fn truncate_reserves_focused_document_chunks_against_structural_context() {
    let structural_document_id = Uuid::now_v7();
    let focused_document_id = Uuid::now_v7();
    let mut chunks = (0..6)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                structural_document_id,
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused_ids = Vec::new();
    for index in 0..2 {
        let mut chunk = setup_focus_runtime_chunk(focused_document_id, index, 500.0);
        chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        focused_ids.push(chunk.chunk_id);
        chunks.push(chunk);
    }
    let query_ir = setup_query_ir("Sample Manifest");

    truncate_chunks_for_context(&mut chunks, 4, Some(&query_ir), &HashSet::new());

    for chunk_id in focused_ids {
        assert!(chunks.iter().any(|chunk| chunk.chunk_id == chunk_id), "{chunks:#?}");
    }
    assert_eq!(chunks.len(), 4);
}

#[test]
fn truncate_does_not_reserve_setup_anchor_for_versioned_procedure_ir() {
    let update_document_id = Uuid::now_v7();
    let setup_document_id = Uuid::now_v7();
    let mut chunks = Vec::new();
    for index in 0..2 {
        let mut chunk = setup_focus_runtime_chunk(
            update_document_id,
            index,
            DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
        );
        chunk.document_label = "Sample Target update guide".to_string();
        chunk.source_text = format!("Step {}. Refresh package version 2.0.{index}.", index + 1);
        chunks.push(chunk);
    }
    let mut setup_anchor =
        setup_focus_runtime_chunk(setup_document_id, 0, DOCUMENT_IDENTITY_SCORE_FLOOR);
    setup_anchor.document_label = "Sample Target setup guide".to_string();
    setup_anchor.source_text =
        "sample-runner --install sample-link\nSettings file /opt/alpha/connector.conf".to_string();
    let setup_anchor_id = setup_anchor.chunk_id;
    chunks.push(setup_anchor);
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

    let setup_anchor = chunks
        .iter()
        .find(|chunk| chunk.chunk_id == setup_anchor_id)
        .expect("setup anchor fixture");
    assert!(
        versioned_update_exact_target_runbook_score("", &query_ir, setup_anchor).is_none(),
        "an unordered package/config anchor is setup evidence, not a versioned runbook"
    );

    truncate_chunks_for_context(&mut chunks, 2, Some(&query_ir), &HashSet::new());

    assert_eq!(chunks.len(), 2);
    assert!(
        !chunks.iter().any(|chunk| chunk.chunk_id == setup_anchor_id),
        "setup/package anchor must not evict versioned procedure evidence"
    );
}
