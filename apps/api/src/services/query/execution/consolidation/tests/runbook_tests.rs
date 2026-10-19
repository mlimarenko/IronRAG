use super::*;

#[test]
fn test_consolidation_evidence_dominance() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(winner_doc, winner_rev, 0, "Dominant doc", 0.9),
            sample_chunk(winner_doc, winner_rev, 1, "Dominant doc", 0.85),
            sample_chunk(winner_doc, winner_rev, 2, "Dominant doc", 0.8),
            sample_chunk(winner_doc, winner_rev, 3, "Dominant doc", 0.75),
            sample_chunk(other_doc, other_rev, 0, "Other doc", 0.6),
            sample_chunk(other_doc, other_rev, 1, "Other doc", 0.55),
        ],
    };

    // Plain descriptive IR with no hint / no subject — only the
    // structural evidence signal can fire.
    let query_ir = ir(QueryScope::SingleDocument);

    let revision_rows: Vec<_> = (0..6)
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner {idx}")))
        .collect();
    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);
    assert_eq!(diag.focus_reason, FocusReason::EvidenceDominance);
    assert_eq!(diag.focused_document_id, Some(winner_doc));
    // Budget cap ≈ 60% of 8 = 5 → winner chunk count should be
    // at most 5 (not the full 6 available).
    assert!(diag.winner_chunk_count <= 5);
}

#[test]
fn test_consolidation_preserves_multi_document_versioned_procedure_anchors() {
    let dominant_doc = Uuid::now_v7();
    let dominant_rev = Uuid::now_v7();
    let procedure_doc = Uuid::now_v7();
    let procedure_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            focused_chunk(dominant_doc, dominant_rev, 0, "Dominant support note", 10_000.0),
            focused_chunk(dominant_doc, dominant_rev, 1, "Dominant support note", 9_999.0),
            focused_chunk(dominant_doc, dominant_rev, 2, "Dominant support note", 9_998.0),
            focused_chunk(dominant_doc, dominant_rev, 3, "Dominant support note", 9_997.0),
            focused_chunk(procedure_doc, procedure_rev, 0, "Upgrade procedure", 9_900.0),
        ],
    };

    let generic_ir = ir(QueryScope::SingleDocument);
    let Some((_, generic_reason, _)) =
        decide_focus(&bundle, &generic_ir, "how to update Alpha Service?", 8)
    else {
        panic!("generic evidence dominance should still consolidate this shape");
    };
    assert_eq!(generic_reason, FocusReason::EvidenceDominance);

    let mut procedure_ir = ir(QueryScope::SingleDocument);
    procedure_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    procedure_ir.target_entities =
        vec![EntityMention { label: "Alpha Service".to_string(), role: EntityRole::Subject }];

    assert!(
        decide_focus(&bundle, &procedure_ir, "how to update Alpha Service?", 8).is_none(),
        "soft evidence dominance must not collapse multi-document focused procedure evidence"
    );
}

#[test]
fn test_consolidation_skips_evidence_dominance_for_single_versioned_procedure_anchor() {
    let dominant_doc = Uuid::now_v7();
    let dominant_rev = Uuid::now_v7();
    let procedure_doc = Uuid::now_v7();
    let procedure_rev = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(dominant_doc, dominant_rev, 0, "Alpha support notes", 0.9),
            sample_chunk(dominant_doc, dominant_rev, 1, "Alpha support notes", 0.85),
            sample_chunk(dominant_doc, dominant_rev, 2, "Alpha support notes", 0.8),
            sample_chunk(dominant_doc, dominant_rev, 3, "Alpha support notes", 0.75),
            focused_chunk(procedure_doc, procedure_rev, 0, "Alpha versioned update guide", 9_900.0),
        ],
    };

    let mut procedure_ir = ir(QueryScope::SingleDocument);
    procedure_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    procedure_ir.target_entities = vec![EntityMention {
        label: "Alpha subject server".to_string(),
        role: EntityRole::Subject,
    }];

    assert!(
        decide_focus(&bundle, &procedure_ir, "how to update Alpha subject server?", 8).is_none(),
        "versioned procedure retrieval must leave the mixed context intact for the deterministic procedure extractor"
    );
}

#[test]
fn test_consolidation_packs_exact_versioned_procedure_runbook() {
    let dominant_doc = Uuid::now_v7();
    let dominant_rev = Uuid::now_v7();
    let runbook_doc = Uuid::now_v7();
    let runbook_rev = Uuid::now_v7();
    let runbook_text = concat!(
        "Alpha Console update runbook\n",
        "1. Run sudo sample-runner update\n",
        "2. Run sudo sample-runner upgrade\n",
        "3. Run sudo sample-configure alpha-rest\n",
        "4. Run sudo service alpha-rest restart"
    );
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            focused_chunk(dominant_doc, dominant_rev, 0, "Alpha Console update note", 20_000.0),
            focused_chunk(dominant_doc, dominant_rev, 1, "Alpha Console update note", 19_999.0),
            focused_chunk(dominant_doc, dominant_rev, 2, "Alpha Console update note", 19_998.0),
            text_chunk(
                runbook_doc,
                runbook_rev,
                3,
                "Alpha Console update runbook",
                1.0,
                runbook_text,
            ),
        ],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types =
        vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha Console".to_string(), role: EntityRole::Subject }];

    let revision_rows = (0..7)
        .map(|idx| {
            let content = if idx == 3 { runbook_text } else { "neighbor update context" };
            chunk_row(runbook_doc, runbook_rev, idx, content)
        })
        .collect::<Vec<_>>();
    let diag = run_apply_with_question(
        &mut bundle,
        &query_ir,
        "how to update Alpha Console?",
        8,
        revision_rows,
    );

    assert_eq!(diag.focus_reason, FocusReason::VersionedProcedureRunbook);
    assert_eq!(diag.focused_document_id, Some(runbook_doc));
    assert_eq!(bundle.chunks.first().map(|chunk| chunk.document_id), Some(runbook_doc));
    assert_eq!(bundle.chunks.first().map(|chunk| chunk.chunk_index), Some(3));
    assert!(bundle.chunks.iter().any(|chunk| {
        chunk.source_text.contains("sample-configure")
            && chunk.source_text.contains("service alpha-rest restart")
    }));
}

#[test]
fn test_consolidation_prefers_command_runbook_over_subject_title_note_for_procedure_query() {
    let subject_doc = Uuid::now_v7();
    let subject_rev = Uuid::now_v7();
    let runbook_doc = Uuid::now_v7();
    let runbook_rev = Uuid::now_v7();
    let runbook_text = concat!(
        "Support Node update procedure\n",
        "1. sample-fetch https://host.invalid/update-bundle.sh -o /tmp/update-bundle.sh\n",
        "2. chmod +x /tmp/update-bundle.sh\n",
        "3. /tmp/update-bundle.sh\n",
        "4. sample-service reload support-node"
    );
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                subject_doc,
                subject_rev,
                0,
                "Support Node overview",
                20_000.0,
                "Support Node overview. The package inventory is described here.",
            ),
            text_chunk(
                subject_doc,
                subject_rev,
                1,
                "Support Node overview",
                19_999.0,
                "Support Node compatibility notes and background facts.",
            ),
            text_chunk(
                subject_doc,
                subject_rev,
                2,
                "Support Node overview",
                19_998.0,
                "Support Node release checklist without executable steps.",
            ),
            text_chunk(runbook_doc, runbook_rev, 21, "Maintenance instructions", 1.0, runbook_text),
        ],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Support Node".to_string(), role: EntityRole::Subject }];
    query_ir.retrieval_query = Some("how to update Support Node".to_string());

    let revision_rows = (18..=24)
        .map(|idx| {
            let content = if idx == 21 { runbook_text } else { "neighbor procedure context" };
            chunk_row(runbook_doc, runbook_rev, idx, content)
        })
        .collect::<Vec<_>>();
    let diag = run_apply_with_question(
        &mut bundle,
        &query_ir,
        "how to update Support Node?",
        8,
        revision_rows,
    );

    assert_eq!(diag.focus_reason, FocusReason::VersionedProcedureRunbook);
    assert_eq!(diag.focused_document_id, Some(runbook_doc));
    assert_eq!(bundle.chunks.first().map(|chunk| chunk.document_id), Some(runbook_doc));
    assert!(bundle.chunks.iter().any(|chunk| chunk.source_text.contains("update-bundle.sh")));
}

#[test]
fn test_consolidation_exact_versioned_procedure_runbook_tie_uses_retrieval_score() {
    let first_doc = Uuid::now_v7();
    let first_rev = Uuid::now_v7();
    let preferred_doc = Uuid::now_v7();
    let preferred_rev = Uuid::now_v7();
    let runbook_text = concat!(
        "Alpha Console update runbook\n",
        "1. sudo sample-runner update\n",
        "2. sudo sample-runner upgrade\n",
        "3. sudo sample-configure alpha-rest\n",
        "4. sudo service alpha-rest restart"
    );
    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(first_doc, first_rev, 1, "Alpha Console update runbook", 10.0, runbook_text),
            text_chunk(
                preferred_doc,
                preferred_rev,
                2,
                "Alpha Console update runbook",
                20.0,
                runbook_text,
            ),
        ],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types =
        vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha Console".to_string(), role: EntityRole::Subject }];

    let Some((winner, reason, _)) =
        decide_focus(&bundle, &query_ir, "how to update Alpha Console?", 8)
    else {
        panic!("exact-target runbook winner expected");
    };

    assert_eq!(reason, FocusReason::VersionedProcedureRunbook);
    assert_eq!(winner.document_id, preferred_doc);
    assert_eq!(winner.ranked_content_anchors.first().copied(), Some(2));
}

#[test]
fn test_consolidation_score_dominance_packs_identity_lane_winner() {
    let winner_doc = Uuid::now_v7();
    let winner_rev = Uuid::now_v7();
    let other_doc = Uuid::now_v7();
    let other_rev = Uuid::now_v7();

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(winner_doc, winner_rev, 0, "Untitled external source", 1_000_000.0),
            sample_chunk(other_doc, other_rev, 0, "Other doc", 50.0),
            sample_chunk(other_doc, other_rev, 1, "Other doc", 49.0),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);
    let revision_rows: Vec<_> = (0..24)
        .map(|idx| chunk_row(winner_doc, winner_rev, idx, &format!("winner chunk {idx}")))
        .collect();

    let diag = run_apply(&mut bundle, &query_ir, 8, revision_rows);

    assert_eq!(diag.focus_reason, FocusReason::ScoreDominance);
    assert_eq!(diag.focused_document_id, Some(winner_doc));
    assert!(diag.winner_chunk_count > 1);
    assert!(diag.winner_chunk_count <= 16);
    assert!(bundle.chunks.iter().all(|chunk| chunk.document_id == winner_doc));
}

#[test]
fn test_consolidation_score_dominance_ignores_normal_score_spread() {
    let doc_a = Uuid::now_v7();
    let rev_a = Uuid::now_v7();
    let doc_b = Uuid::now_v7();
    let rev_b = Uuid::now_v7();

    let bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(doc_a, rev_a, 0, "Doc A", 1.0),
            sample_chunk(doc_b, rev_b, 0, "Doc B", 0.5),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    assert!(decide_focus(&bundle, &query_ir, DEFAULT_TEST_QUESTION, 8).is_none());
}

#[test]
fn test_consolidation_score_dominance_requires_identity_scale_and_finite_scores() {
    let doc_a = Uuid::now_v7();
    let rev_a = Uuid::now_v7();
    let doc_b = Uuid::now_v7();
    let rev_b = Uuid::now_v7();

    let below_identity_scale = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(doc_a, rev_a, 0, "Doc A", 1_000.0),
            sample_chunk(doc_b, rev_b, 0, "Doc B", 0.5),
        ],
    };
    let query_ir = ir(QueryScope::SingleDocument);

    assert!(decide_focus(&below_identity_scale, &query_ir, DEFAULT_TEST_QUESTION, 8).is_none());

    let non_finite = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(doc_a, rev_a, 0, "Doc A", f32::INFINITY),
            sample_chunk(doc_b, rev_b, 0, "Doc B", 1.0),
        ],
    };

    assert!(decide_focus(&non_finite, &query_ir, DEFAULT_TEST_QUESTION, 8).is_none());
}
