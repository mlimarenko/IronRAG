use super::*;

#[test]
fn test_consolidation_only_retrieved_document_fetches_neighbours() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Only matched guide", 0.9)],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    let revision_rows: Vec<_> = (0..24)
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
        .collect();
    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
    assert_eq!(diag.focused_document_id, Some(winner_doc));
    assert!(
        diag.winner_chunk_count > 1,
        "single-document retrieval must fetch neighbouring chunks"
    );
    assert!(diag.winner_chunk_count <= 16);
    assert!(bundle.chunks.iter().all(|chunk| chunk.document_id == winner_doc));
}

#[test]
fn test_consolidation_ignores_source_profile_as_sparse_anchor() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            source_profile_chunk(winner_doc, winner_rev, 0, "records.jsonl", 1_000_000.0),
            sample_chunk(winner_doc, winner_rev, 500, "records.jsonl", 1000.0),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);
    let mut revision_rows = vec![chunk_row_with_kind(
        winner_doc,
        winner_rev,
        0,
        Some("source_profile"),
        "[source_profile source_format=record_jsonl sequence_kind=record_stream unit_count=600]",
    )];
    revision_rows.extend(
        (0..520).map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("source chunk {idx}"))),
    );

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

    assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
    let indices = bundle.chunks.iter().map(|chunk| chunk.chunk_index).collect::<Vec<_>>();
    assert!(
        indices.contains(&500),
        "late content anchor must survive winner materialization: {indices:?}"
    );
    assert!(
        !indices.iter().all(|index| *index < 16),
        "source profile at 0 must not expand into a head-only 0..15 pack: {indices:?}"
    );
}

#[test]
fn test_consolidation_prioritizes_ranked_anchors_before_neighbors() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(winner_doc, winner_rev, 500, "records.jsonl", 0.95),
            sample_chunk(winner_doc, winner_rev, 120, "records.jsonl", 0.90),
        ],
    };
    let winner = DocumentAggregate {
        document_id: winner_doc,
        revision_id: winner_rev,
        evidence_count: 2,
        best_score: 0.95,
        content_anchors: [120, 500].into_iter().collect(),
        ranked_content_anchors: vec![500, 120],
    };
    let revision_rows = [119, 120, 121, 499, 500, 501]
        .into_iter()
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("source chunk {idx}")))
        .collect::<Vec<_>>();

    let diag = apply_winner_chunks(
        &mut bundle,
        &winner,
        FocusReason::OnlyRetrievedDocument,
        2,
        2,
        revision_rows,
    );

    assert_eq!(diag.winner_chunk_count, 2);
    let indices = bundle.chunks.iter().map(|chunk| chunk.chunk_index).collect::<Vec<_>>();
    assert_eq!(indices, vec![500, 120]);
}

#[test]
fn test_consolidation_ranks_single_document_anchors_by_question_focus() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let question = "Which record mentions AlphaKey on 2024-09-03?";
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                winner_doc,
                winner_rev,
                40,
                "records.jsonl",
                1.0,
                "general record summary without the requested identifier",
            ),
            text_chunk(
                winner_doc,
                winner_rev,
                4,
                "records.jsonl",
                1.0,
                "record body: AlphaKey was listed on 2024-09-03 with supporting details",
            ),
            text_chunk(
                winner_doc,
                winner_rev,
                120,
                "records.jsonl",
                1.0,
                "record body: unrelated 2024 status note",
            ),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    let Some((winner, reason, budget)) = decide_focus(&bundle, &query_ir, question, 8) else {
        panic!("single retrieved document should consolidate");
    };
    assert_eq!(reason, FocusReason::OnlyRetrievedDocument);
    assert_eq!(winner.ranked_content_anchors.first().copied(), Some(4));
    assert!(!winner_anchor_windows(&winner, budget).is_empty());

    let revision_rows = (0..128)
        .map(|idx| {
            let text = if idx == 4 {
                "record body: AlphaKey was listed on 2024-09-03 with supporting details"
            } else {
                "neighbor record"
            };
            chunk_row(winner_doc, winner_rev, idx, text)
        })
        .collect::<Vec<_>>();
    let diag = run_apply_with_question(&mut bundle, &query_ir, question, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
    assert_eq!(bundle.chunks.first().map(|chunk| chunk.chunk_index), Some(4));
}

#[test]
fn test_consolidation_anchor_ranking_prefers_query_focus_before_tiny_score_gap() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let question = "Which record mentions AlphaKey?";
    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                winner_doc,
                winner_rev,
                40,
                "records.jsonl",
                2.0,
                "high confidence retrieved record",
            ),
            text_chunk(
                winner_doc,
                winner_rev,
                4,
                "records.jsonl",
                1.0,
                "lower confidence record mentioning AlphaKey",
            ),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    let Some((winner, _, _)) = decide_focus(&bundle, &query_ir, question, 8) else {
        panic!("single retrieved document should consolidate");
    };
    assert_eq!(winner.ranked_content_anchors.first().copied(), Some(4));
}

#[test]
fn test_consolidation_source_profile_only_retrieval_noops() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![source_profile_chunk(winner_doc, winner_rev, 0, "records.jsonl", 1_000_000.0)],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    assert!(decide_focus(&bundle, &query_ir, DEFAULT_TEST_QUESTION, 8).is_none());
}

#[test]
fn test_consolidation_winner_excerpt_preserves_late_technical_literal() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Only matched guide", 0.9)],
    };
    let query_ir = ir(QueryScope::SingleDocument);
    let late_literal = "callbackEndpoint=https://localhost.example/internal/api";
    let long_content = format!("{} {late_literal}", "context ".repeat(70));
    let revision_rows = vec![chunk_row(winner_doc, winner_rev, 0, &long_content)];

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

    assert_eq!(diag.focus_reason, FocusReason::OnlyRetrievedDocument);
    assert!(
        bundle.chunks[0].excerpt.contains(late_literal),
        "winner excerpts must not truncate late technical literals"
    );
}

#[test]
fn test_consolidation_tie_no_action() {
    let doc_a = Uuid::now_v7();
    let doc_b = Uuid::now_v7();
    let rev_a = Uuid::now_v7();
    let rev_b = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(doc_a, rev_a, 0, "Alpha guide", 0.9),
            sample_chunk(doc_a, rev_a, 1, "Alpha guide", 0.85),
            sample_chunk(doc_a, rev_a, 2, "Alpha guide", 0.8),
            sample_chunk(doc_b, rev_b, 0, "Bravo guide", 0.88),
            sample_chunk(doc_b, rev_b, 1, "Bravo guide", 0.82),
        ],
    };
    let original_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();

    // No IR hint, no subject — evidence ratio 3/2 = 1.5 < 2.0 cutoff.
    let query_ir = ir(QueryScope::SingleDocument);
    let diag = run_apply(&mut bundle, &query_ir, 8, Vec::new());
    assert_eq!(diag.focus_reason, FocusReason::None);
    let after_ids: Vec<Uuid> = bundle.chunks.iter().map(|c| c.chunk_id).collect();
    assert_eq!(after_ids, original_ids);
}

#[test]
fn test_consolidation_budget_cap() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9),
            sample_chunk(other_doc, other_rev, 0, "Tangent 1", 0.85),
            sample_chunk(other_doc, other_rev, 1, "Tangent 1", 0.8),
            sample_chunk(other_doc, other_rev, 2, "Tangent 1", 0.75),
            sample_chunk(other_doc, other_rev, 3, "Tangent 1", 0.7),
            sample_chunk(other_doc, other_rev, 4, "Tangent 1", 0.65),
            sample_chunk(other_doc, other_rev, 5, "Tangent 1", 0.6),
        ],
    };

    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

    // Revision only has 3 chunks total — even with full budget,
    // winner_chunk_count must be capped at 3, remaining 5 slots
    // get top tangentials.
    let revision_rows = vec![
        chunk_row(winner_doc, winner_rev, 0, "intro"),
        chunk_row(winner_doc, winner_rev, 1, "config"),
        chunk_row(winner_doc, winner_rev, 2, "verify"),
    ];

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
    assert_eq!(diag.winner_chunk_count, 3);
    assert_eq!(diag.tangential_chunk_count, 5);
    assert_eq!(bundle.chunks.len(), 8);

    // First three are winner chunks sorted by index; rest are tangentials.
    for (slot, chunk) in bundle.chunks.iter().take(3).enumerate() {
        assert_eq!(chunk.document_id, winner_doc, "slot {slot} must be winner");
        assert_eq!(chunk.chunk_index, slot as i32);
    }
    for chunk in bundle.chunks.iter().skip(3) {
        assert_eq!(chunk.document_id, other_doc);
    }
}

#[test]
fn test_consolidation_explicit_single_doc_extended_budget() {
    // When the compiler explicitly pins one document via a hint
    // and that document has a long config / contract body, the
    // winner pack is allowed to exceed `top_k` so the LLM sees
    // the whole anchor-window instead of an arbitrary 8-chunk
    // slice truncated in the middle of a section.
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9),
            sample_chunk(other_doc, other_rev, 0, "Tangent", 0.6),
            sample_chunk(other_doc, other_rev, 1, "Tangent", 0.55),
        ],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

    // Revision has 24 chunks — strictly more than `top_k = 8` and
    // more than `FOCUSED_WINNER_MAX_CHUNKS = 16`.
    let revision_rows: Vec<_> = (0..24)
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
    assert_eq!(diag.focused_document_id, Some(winner_doc));
    assert!(
        diag.winner_chunk_count > 8,
        "winner must be allowed to exceed top_k = 8 under explicit focus, got {}",
        diag.winner_chunk_count
    );
    assert!(
        diag.winner_chunk_count <= 16,
        "winner budget must stay inside FOCUSED_WINNER_MAX_CHUNKS = 16"
    );
    // Bundle stays inside the explicit-focus cap.
    assert!(bundle.chunks.len() <= 16, "bundle must not exceed explicit focus cap");
}

#[test]
fn test_consolidation_explicit_budget_is_hard_cap() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![sample_chunk(winner_doc, winner_rev, 0, "Provider A Admin Guide", 0.9)],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.document_focus = Some(DocumentHint { hint: "provider a".to_string() });

    let revision_rows: Vec<_> = (0..32)
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 32, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::DocumentFocusHint);
    assert_eq!(diag.winner_chunk_count, 16);
    assert_eq!(bundle.chunks.len(), 16);
}
