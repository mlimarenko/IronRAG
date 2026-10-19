use super::test_prelude::*;

#[test]
fn setup_focus_rows_select_package_plus_configuration_path() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(workspace_id, library_id, document_id, revision_id, 0, "Subject Alpha overview"),
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "Install the module:\nsample-runner --install sample-link\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/connector/connector.conf.",
        ),
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            2,
            "Example display file /opt/alpha/display.ini.",
        ),
    ];

    let selected = select_setup_focus_document_rows(&rows);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2]);
    assert!(setup_focus_row_score(&selected[0]) > setup_focus_row_score(&selected[1]));
}

#[test]
fn setup_focus_rows_select_configuration_parameter_blocks_without_package_command() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(workspace_id, library_id, document_id, revision_id, 0, "Subject Alpha overview"),
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "[Connector]\nendpoint = https://127.0.0.1/api\nretryTimeout = 15\nFile: /opt/alpha/connector/settings.conf",
        ),
    ];

    let selected = select_setup_focus_document_rows(&rows);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1]);
    assert!(setup_focus_row_score(&selected[0]) > 0);
}

#[test]
fn setup_focus_rows_keep_parameter_table_after_setup_anchor() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Install the module:\nsample-runner --install sample-link\nSettings are stored in /opt/alpha/connector/connector.conf.",
        ),
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "Sheet: Connector settings | Row 1 | Name: partnerId | Type: string | Description: Partner identifier",
        ),
        chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            2,
            "Sheet: Connector settings | Row 2 | Name: credentialToken | Type: string | Description: Shared credential",
        ),
    ];

    let selected = select_setup_focus_document_rows(&rows);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [0, 1, 2]);
}

#[test]
fn setup_focus_rows_keep_late_parameter_table_after_setup_anchor() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut rows = vec![chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        0,
        "Install the module:\nsample-runner --install sample-link\nSettings are stored in /opt/alpha/connector/connector.conf.",
    )];
    for index in 1..20 {
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            index,
            "Configuration table continuation",
        ));
    }
    rows.push(chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        20,
        "| sendDetails | boolean | Send detailed workflow payload | Default true |",
    ));

    let selected = select_setup_focus_document_rows(&rows);

    assert!(selected.iter().any(|row| row.chunk_index == 20));
}

#[test]
fn setup_focus_chunk_score_prioritizes_parameter_context_above_exact_identity_hits() {
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 12,
        chunk_kind: Some("code_block".to_string()),
        document_id: Uuid::now_v7(),
        document_label: "Connector Alpha".to_string(),
        excerpt: "mode\ncredentialToken\nprimaryKey".to_string(),
        score: Some(1.0),
        score_kind: RuntimeChunkScoreKind::Relevance,
        source_text: "mode\ncredentialToken\nprimaryKey".to_string(),
    };

    let score = setup_focus_document_chunk_score(&chunk);

    assert!(score > DOCUMENT_IDENTITY_SCORE_FLOOR * 10.0);
}

#[test]
fn setup_focus_row_score_includes_structured_parameter_table_rows() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let row = chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        21,
        "Sheet: Connector settings | Table: Workflow details | Row 1 | Name: fillWorkflowDetails | Type: boolean | Values: true false | Description: Fill workflowDetails | Default: true",
    );

    assert!(setup_focus_row_score(&row) > 0);
}

#[test]
fn setup_focus_document_chunks_preserve_best_document_context_before_noisy_matches() {
    let noisy_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let noisy_chunks = (0..SETUP_FOCUS_DOCUMENT_CHUNK_CAP)
        .map(|index| setup_focus_runtime_chunk(noisy_document_id, index as i32, 1.0))
        .collect::<Vec<_>>();
    let target_chunks = vec![
        setup_focus_runtime_chunk(target_document_id, 1, SETUP_FOCUS_DOCUMENT_SCORE_BASE),
        setup_focus_runtime_chunk(target_document_id, 20, SETUP_FOCUS_DOCUMENT_SCORE_BASE),
    ];

    let selected = select_setup_focus_document_chunks(vec![
        (1, noisy_document_id, noisy_chunks),
        (100, target_document_id, target_chunks),
    ]);

    assert_eq!(
        selected
            .iter()
            .take(2)
            .map(|chunk| (chunk.document_id, chunk.chunk_index))
            .collect::<Vec<_>>(),
        vec![(target_document_id, 1), (target_document_id, 20)]
    );
}

#[test]
fn diversification_preserves_setup_focus_document_inventory() {
    let setup_document_id = Uuid::now_v7();
    let ordinary_document_id = Uuid::now_v7();
    let mut chunks = (0..6)
        .map(|index| {
            setup_focus_runtime_chunk(
                setup_document_id,
                index,
                SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
            )
        })
        .collect::<Vec<_>>();
    chunks.extend((0..6).map(|index| {
        setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
    }));
    let setup_focus_document_ids = BTreeSet::from([setup_document_id]);

    let selected =
        diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT, &setup_focus_document_ids);

    assert_eq!(selected.iter().filter(|chunk| chunk.document_id == setup_document_id).count(), 6);
    assert_eq!(
        selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
        MAX_CHUNKS_PER_DOCUMENT
    );
}

#[test]
fn diversification_preserves_protected_document_evidence_inventory() {
    let protected_document_id = Uuid::now_v7();
    let ordinary_document_id = Uuid::now_v7();
    let mut chunks = (0..4)
        .map(|index| {
            let mut chunk = setup_focus_runtime_chunk(
                protected_document_id,
                index,
                document_identity_chunk_score(0, index as usize),
            );
            chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
            chunk
        })
        .collect::<Vec<_>>();
    chunks.extend((0..4).map(|index| {
        setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
    }));
    let protected_document_ids = BTreeSet::from([protected_document_id]);

    let selected =
        diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT, &protected_document_ids);

    assert_eq!(
        selected.iter().filter(|chunk| chunk.document_id == protected_document_id).count(),
        4
    );
    assert_eq!(
        selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
        MAX_CHUNKS_PER_DOCUMENT
    );
}

#[test]
fn retain_scoped_documents_keeps_protected_document_evidence() {
    let scoped_document_id = Uuid::now_v7();
    let protected_document_id = Uuid::now_v7();
    let unrelated_document_id = Uuid::now_v7();
    let protected_chunk =
        setup_focus_runtime_chunk(protected_document_id, 0, document_identity_chunk_score(0, 0));
    let protected_chunk_id = protected_chunk.chunk_id;
    let mut chunks = vec![
        setup_focus_runtime_chunk(scoped_document_id, 0, 1.0),
        protected_chunk,
        setup_focus_runtime_chunk(unrelated_document_id, 0, 0.5),
    ];

    retain_scoped_documents(
        &mut chunks,
        &BTreeSet::from([scoped_document_id]),
        &BTreeSet::new(),
        &BTreeSet::from([protected_document_id]),
    );

    assert!(chunks.iter().any(|chunk| chunk.document_id == scoped_document_id));
    assert!(chunks.iter().any(|chunk| chunk.chunk_id == protected_chunk_id));
    assert!(!chunks.iter().any(|chunk| chunk.document_id == unrelated_document_id));
}

#[test]
fn diversification_uses_global_multi_document_setup_focus_budget() {
    let setup_document_ids = (0..3).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let chunks = (0..12)
        .flat_map(|index| {
            setup_document_ids.iter().map(move |document_id| {
                setup_focus_runtime_chunk(
                    *document_id,
                    index,
                    SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
                )
            })
        })
        .collect::<Vec<_>>();
    let setup_focus_document_ids = setup_document_ids.iter().copied().collect::<BTreeSet<_>>();

    let selected =
        diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT, &setup_focus_document_ids);

    assert_eq!(
        selected
            .iter()
            .filter(|chunk| setup_focus_document_ids.contains(&chunk.document_id))
            .count(),
        SETUP_FOCUS_DOCUMENT_CHUNK_CAP
    );
    for document_id in setup_document_ids {
        let selected_for_document =
            selected.iter().filter(|chunk| chunk.document_id == document_id).count();
        assert!(
            selected_for_document > MAX_CHUNKS_PER_DOCUMENT,
            "setup focus budget should keep more than the ordinary per-document cap"
        );
        assert!(
            selected_for_document <= SETUP_FOCUS_DOCUMENT_CHUNK_CAP,
            "setup focus budget remains per-document bounded"
        );
    }
}

#[test]
fn diversification_caps_ordinary_documents_even_with_setup_focus_budget() {
    let setup_document_id = Uuid::now_v7();
    let ordinary_document_id = Uuid::now_v7();
    let mut chunks = (0..12)
        .map(|index| {
            setup_focus_runtime_chunk(
                setup_document_id,
                index,
                SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
            )
        })
        .collect::<Vec<_>>();
    chunks.extend((0..12).map(|index| {
        setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
    }));
    let setup_focus_document_ids = BTreeSet::from([setup_document_id]);

    let selected =
        diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT, &setup_focus_document_ids);

    assert_eq!(selected.iter().filter(|chunk| chunk.document_id == setup_document_id).count(), 12);
    assert_eq!(
        selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
        MAX_CHUNKS_PER_DOCUMENT
    );
}

#[test]
fn setup_focus_document_score_prefers_literal_matching_parameter_rows() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let first_document = Uuid::now_v7();
    let second_document = Uuid::now_v7();
    let first_revision = Uuid::now_v7();
    let second_revision = Uuid::now_v7();
    let generic_rows = vec![
        chunk_row(
            workspace_id,
            library_id,
            first_document,
            first_revision,
            1,
            "Install with sample-runner --install sample-link. Configure /opt/alpha/alpha.conf.",
        ),
        chunk_row(
            workspace_id,
            library_id,
            first_document,
            first_revision,
            2,
            "Sheet: Settings | Row 1 | Name: url | Type: string",
        ),
    ];
    let matching_rows = vec![
        chunk_row(
            workspace_id,
            library_id,
            second_document,
            second_revision,
            1,
            "Install with sample-runner --install sample-link. Configure /opt/alpha/alpha.conf.",
        ),
        chunk_row(
            workspace_id,
            library_id,
            second_document,
            second_revision,
            2,
            "Sheet: Settings | Row 2 | Name: primaryKey | Type: string",
        ),
        chunk_row(
            workspace_id,
            library_id,
            second_document,
            second_revision,
            3,
            "Sheet: Settings | Row 3 | Name: credentialToken | Type: string",
        ),
    ];
    let mut query_ir = setup_query_ir("Subject Alpha");
    query_ir.literal_constraints = vec![
        LiteralSpan {
            text: "primary".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        },
        LiteralSpan {
            text: "secret".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        },
    ];

    assert!(
        setup_focus_document_candidate_score(&matching_rows, &query_ir)
            > setup_focus_document_candidate_score(&generic_rows, &query_ir)
    );
}

#[test]
fn setup_focus_document_label_identity_score_prefers_full_subject_coverage() {
    let generic = document_row("subject-alpha.md", "Subject Alpha setup reference");
    let focused = document_row("subject-beta.md", "Subject Beta setup reference");
    let mut query_ir = setup_query_ir("Subject");
    query_ir.document_focus = None;
    query_ir.target_entities = vec![
        EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "Beta".to_string(), role: EntityRole::Subject },
    ];

    assert!(
        setup_focus_document_label_identity_score(&focused, &query_ir)
            > setup_focus_document_label_identity_score(&generic, &query_ir)
    );
}

#[test]
fn setup_focus_candidates_keep_long_manual_after_many_short_matches() {
    let target = document_row(
        "environment-alpha-admin-guide.md",
        "Subject Alpha - Administrator setup manual",
    );
    let mut documents = vec![target.clone()];
    for index in 0..64 {
        documents.push(document_row(
            &format!("image-{index}.png"),
            &format!("Subject Alpha: subject screenshot {index}"),
        ));
    }
    let document_index = documents
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();
    let query_ir = setup_query_ir("Subject Alpha");

    let candidates = setup_focus_candidate_document_ids(
        &query_ir,
        &document_index,
        SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
    );

    assert!(candidates.contains(&target.document_id));
}

#[test]
fn untyped_entity_identity_does_not_activate_setup_focus_lane() {
    let mut query_ir = setup_query_ir("");
    query_ir.act = QueryAct::Describe;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.target_types.clear();
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha Connector".to_string(), role: EntityRole::Subject }];
    query_ir.document_focus = None;
    query_ir.confidence = 0.25;

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}
