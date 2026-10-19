use super::*;

#[test]
fn test_consolidation_explicit_single_doc() {
    let subject_doc = Uuid::now_v7();
    let subject_rev = Uuid::now_v7();
    let tangential_doc = Uuid::now_v7();
    let tangential_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(subject_doc, subject_rev, 0, "Subject Alpha Admin Guide", 0.9),
            sample_chunk(tangential_doc, tangential_rev, 0, "Provider B Manual", 0.8),
            sample_chunk(tangential_doc, tangential_rev, 1, "Provider B Manual", 0.7),
            sample_chunk(tangential_doc, tangential_rev, 2, "Provider B Manual", 0.6),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];

    // Winner revision has 6 chunks (0..=5). Fetched range will
    // respect budget = 75% of 8 = 6 slots.
    let revision_rows: Vec<_> = (0..6)
        .map(|idx| chunk_row(subject_doc, subject_rev, idx, &format!("winner chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::SingleDocumentSubject);
    assert_eq!(diag.focused_document_id, Some(subject_doc));
    assert!(diag.winner_chunk_count >= 6);

    // Winner chunks come first and are sorted by chunk_index ascending.
    let winner_slice: Vec<_> = bundle.chunks.iter().take(diag.winner_chunk_count).collect();
    for chunk in &winner_slice {
        assert_eq!(chunk.document_id, subject_doc);
        assert_eq!(chunk.revision_id, subject_rev);
    }
    let indices: Vec<i32> = winner_slice.iter().map(|c| c.chunk_index).collect();
    let expected: Vec<i32> = (*indices.first().unwrap()..=*indices.last().unwrap()).collect();
    assert_eq!(indices, expected, "winner chunks must be contiguous and sorted by chunk_index");
    // Budget = max(top_k, FOCUSED_WINNER_MAX_CHUNKS) = 16 for top_k=8,
    // but the revision only has 6 chunks; tangentials fill the
    // `top_k - winner_chunk_count` residue so the bundle never
    // exceeds top_k when the winner pack stays under it.
    assert!(bundle.chunks.len() <= 8);
}

#[test]
fn test_consolidation_document_focus_hint() {
    let hint_doc = Uuid::now_v7();
    let hint_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(hint_doc, hint_rev, 0, "Payment Module Provider A Admin", 0.6),
            sample_chunk(other_doc, other_rev, 0, "Platform POS Manual", 0.9),
            sample_chunk(other_doc, other_rev, 1, "Platform POS Manual", 0.85),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

    let revision_rows: Vec<_> = (0..8)
        .map(|idx| chunk_row(hint_doc, hint_rev, idx, &format!("hint chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
    assert_eq!(diag.focused_document_id, Some(hint_doc));
    assert_eq!(diag.winner_chunk_count, 8);
    assert_eq!(diag.tangential_chunk_count, 0);
    for chunk in &bundle.chunks {
        assert_eq!(chunk.document_id, hint_doc);
    }
}

#[test]
fn test_consolidation_document_focus_does_not_pack_enumerate_scope() {
    let focus_doc = Uuid::now_v7();
    let focus_rev = Uuid::now_v7();
    let companion_doc = Uuid::now_v7();
    let companion_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(focus_doc, focus_rev, 0, "Alpha Suite overview", 0.91),
            sample_chunk(companion_doc, companion_rev, 0, "Alpha Suite connector catalog", 0.9),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.act = QueryAct::Enumerate;
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha Suite".to_string() });
    query_ir.target_entities =
        vec![EntityMention { label: "connector options".to_string(), role: EntityRole::Object }];

    assert!(
        decide_focus(&bundle, &query_ir, "Enumerate connector options.", 8).is_none(),
        "enumeration must preserve companion documents unless IR explicitly targets a document"
    );
}

#[test]
fn test_consolidation_document_focus_does_not_pack_compare_concept_scope() {
    let focus_doc = Uuid::now_v7();
    let focus_rev = Uuid::now_v7();
    let companion_doc = Uuid::now_v7();
    let companion_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(focus_doc, focus_rev, 0, "Alpha Suite provider catalog", 0.91),
            sample_chunk(companion_doc, companion_rev, 0, "Alpha Suite fallback guide", 0.9),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.act = QueryAct::Compare;
    query_ir.target_types = vec![QueryTargetKind::Concept];
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha Suite".to_string() });
    query_ir.target_entities = vec![
        EntityMention { label: "provider catalog".to_string(), role: EntityRole::Object },
        EntityMention { label: "fallback guide".to_string(), role: EntityRole::Object },
    ];

    assert!(
        decide_focus(&bundle, &query_ir, "Compare provider and fallback options.", 8).is_none(),
        "concept comparisons must not collapse multi-topic retrieval into one hinted document"
    );
}

#[test]
fn test_consolidation_evidence_dominance_does_not_pack_enumerate_scope() {
    let dominant_doc = Uuid::now_v7();
    let dominant_rev = Uuid::now_v7();
    let companion_doc = Uuid::now_v7();
    let companion_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(dominant_doc, dominant_rev, 0, "Alpha Suite overview", 0.9),
            sample_chunk(dominant_doc, dominant_rev, 1, "Alpha Suite overview", 0.86),
            sample_chunk(dominant_doc, dominant_rev, 2, "Alpha Suite overview", 0.82),
            sample_chunk(dominant_doc, dominant_rev, 3, "Alpha Suite overview", 0.78),
            sample_chunk(companion_doc, companion_rev, 0, "Alpha Suite connector catalog", 0.7),
            sample_chunk(companion_doc, companion_rev, 1, "Alpha Suite connector catalog", 0.66),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.act = QueryAct::Enumerate;
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha Suite".to_string() });

    assert!(
        decide_focus(&bundle, &query_ir, "Enumerate connector options.", 8).is_none(),
        "evidence dominance must not override library-wide recall for broad enumerations"
    );
}

#[test]
fn test_consolidation_document_focus_packs_enumerate_document_target() {
    let focus_doc = Uuid::now_v7();
    let focus_rev = Uuid::now_v7();
    let companion_doc = Uuid::now_v7();
    let companion_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(focus_doc, focus_rev, 0, "Alpha Suite overview", 0.91),
            sample_chunk(companion_doc, companion_rev, 0, "Alpha Suite connector catalog", 0.9),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Document];
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha Suite overview".to_string() });

    let revision_rows: Vec<_> = (0..4)
        .map(|idx| chunk_row(focus_doc, focus_rev, idx, &format!("overview chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
    assert_eq!(diag.focused_document_id, Some(focus_doc));
}

#[test]
fn test_consolidation_rejects_weak_single_token_subject_overlap() {
    let folder_doc = Uuid::now_v7();
    let folder_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(folder_doc, folder_rev, 0, "Shared folder", 0.8),
            sample_chunk(other_doc, other_rev, 0, "Operations manual", 0.7),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities = vec![EntityMention {
        label: "scan folder network configuration commands".to_string(),
        role: EntityRole::Subject,
    }];

    let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());

    assert_eq!(diag.focus_reason, FocusReason::None);
    assert_eq!(diag.focused_document_id, None);
}

#[test]
fn test_consolidation_rejects_long_descriptive_subject_pin() {
    let services_doc = Uuid::now_v7();
    let services_rev = Uuid::now_v7();
    let compose_doc = Uuid::now_v7();
    let compose_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(services_doc, services_rev, 0, "Kubernetes Services", 0.95),
            sample_chunk(compose_doc, compose_rev, 0, "Sample Manifest Microservices", 0.9),
            sample_chunk(compose_doc, compose_rev, 1, "Sample Manifest Microservices", 0.8),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities = vec![EntityMention {
        label: "docker compose services and ports".to_string(),
        role: EntityRole::Subject,
    }];

    let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());

    assert_eq!(diag.focus_reason, FocusReason::None);
    assert_eq!(diag.focused_document_id, None);
}

#[test]
fn test_consolidation_subject_match_tolerates_single_token_edit() {
    let subject_doc = Uuid::now_v7();
    let subject_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(subject_doc, subject_rev, 0, "Connector TargetName Guide", 0.6),
            sample_chunk(other_doc, other_rev, 0, "Connector Alpha Guide", 0.9),
            sample_chunk(other_doc, other_rev, 1, "Connector Alpha Guide", 0.8),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "TargetNme".to_string(), role: EntityRole::Subject }];

    let revision_rows: Vec<_> = (0..8)
        .map(|idx| chunk_row(subject_doc, subject_rev, idx, &format!("subject chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

    assert_eq!(diag.focus_reason, FocusReason::SingleDocumentSubject);
    assert_eq!(diag.focused_document_id, Some(subject_doc));
}

#[test]
fn test_consolidation_query_focus_evidence_packs_ambiguous_title_winner() {
    let focused_doc = Uuid::now_v7();
    let focused_rev = Uuid::now_v7();
    let sibling_doc = Uuid::now_v7();
    let sibling_rev = Uuid::now_v7();
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "Variant Red".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "redMode".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Identifier,
    }];
    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                sibling_doc,
                sibling_rev,
                0,
                "Shared setup",
                80.0,
                "Variant Blue uses blueMode and unrelated sibling values.",
            ),
            text_chunk(
                focused_doc,
                focused_rev,
                0,
                "Shared setup",
                10.0,
                "Variant Red setup uses redMode and the selected procedure.",
            ),
        ],
    };

    let Some((winner, reason, _)) = decide_focus(&bundle, &query_ir, "configure Variant Red", 8)
    else {
        panic!("query-focus evidence winner expected");
    };

    assert_eq!(reason, FocusReason::QueryFocusEvidence);
    assert_eq!(winner.document_id, focused_doc);
}

#[test]
fn test_consolidation_query_focus_evidence_rejects_ties() {
    let left_doc = Uuid::now_v7();
    let left_rev = Uuid::now_v7();
    let right_doc = Uuid::now_v7();
    let right_rev = Uuid::now_v7();
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "Variant Red".to_string(), role: EntityRole::Subject }];
    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(left_doc, left_rev, 0, "Shared setup", 80.0, "Variant Red setup overview."),
            text_chunk(
                right_doc,
                right_rev,
                0,
                "Shared setup",
                70.0,
                "Variant Red setup checklist.",
            ),
        ],
    };

    assert!(decide_focus(&bundle, &query_ir, "configure Variant Red", 8).is_none());
}

#[test]
fn test_consolidation_multi_document_negative() {
    let doc_a = Uuid::now_v7();
    let doc_b = Uuid::now_v7();
    let rev_a = Uuid::now_v7();
    let rev_b = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(doc_a, rev_a, 0, "Doc A", 0.9),
            sample_chunk(doc_a, rev_a, 1, "Doc A", 0.8),
            sample_chunk(doc_b, rev_b, 0, "Doc B", 0.7),
            sample_chunk(doc_b, rev_b, 1, "Doc B", 0.6),
        ],
    };
    let original_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();

    let mut query_ir = ir(QueryScope::MultiDocument);
    query_ir.target_entities =
        vec![EntityMention { label: "Doc A".to_string(), role: EntityRole::Subject }];

    let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());
    assert_eq!(diag.focus_reason, FocusReason::None);
    assert_eq!(diag.focused_document_id, None);
    let after_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();
    assert_eq!(after_ids, original_ids, "bundle must be untouched on multi-document scope");
}

#[test]
fn setup_anchor_variants_skip_single_document_subject_consolidation() {
    let alpha_doc = Uuid::now_v7();
    let alpha_rev = Uuid::now_v7();
    let beta_doc = Uuid::now_v7();
    let beta_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                alpha_doc,
                alpha_rev,
                0,
                "QRP Alpha setup",
                120.0,
                "sample-install alpha-qrp\nSettings file: /opt/qrp/alpha/alpha.ini",
            ),
            text_chunk(
                beta_doc,
                beta_rev,
                0,
                "QRP Beta setup",
                110.0,
                "sample-install beta-qrp\nSettings file: /opt/qrp/beta/beta.conf",
            ),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.target_entities =
        vec![EntityMention { label: "QRP".to_string(), role: EntityRole::Subject }];

    assert!(decide_focus(&bundle, &query_ir, "how to configure QRP?", 8).is_none());
}
