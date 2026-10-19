use super::test_prelude::*;

#[test]
fn truncate_bundle_preserves_runtime_evidence_lanes() {
    let mut high_scored_noise = (0..8)
        .map(|index| runtime_chunk(&format!("noise-{index}"), 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut graph_evidence = runtime_chunk("rare graph evidence", graph_evidence_chunk_score(0));
    graph_evidence.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    high_scored_noise.push(graph_evidence.clone());
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: high_scored_noise,
    };

    truncate_bundle(&mut bundle, 4, None, &std::collections::HashSet::new());

    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == graph_evidence.chunk_id));
    assert_eq!(bundle.chunks[0].chunk_id, graph_evidence.chunk_id);
}

#[test]
fn truncate_bundle_preserves_content_anchor_evidence_lanes() {
    let mut high_scored_noise = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut content_anchor = runtime_chunk("focused body evidence", 1.0);
    content_anchor.score_kind = RuntimeChunkScoreKind::ContentAnchor;
    let content_anchor_id = content_anchor.chunk_id;
    high_scored_noise.push(content_anchor);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: high_scored_noise,
    };

    truncate_bundle(&mut bundle, 4, None, &std::collections::HashSet::new());

    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == content_anchor_id),
        "content-anchor evidence must survive final context truncation: {:#?}",
        bundle.chunks
    );
}

#[test]
fn truncate_bundle_orders_same_lane_by_score_before_insertion_order() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused = runtime_chunk("focused setup parameter table", DOCUMENT_IDENTITY_SCORE_FLOOR);
    focused.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 1_000.0);
    focused.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    let focused_id = focused.chunk_id;
    chunks.push(focused);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, None, &std::collections::HashSet::new());

    assert_eq!(bundle.chunks[0].chunk_id, focused_id);
}

#[test]
fn truncate_bundle_uses_semantic_rank_before_raw_score_within_the_same_safe_lane() {
    let mut raw_score_winner = runtime_chunk("raw-score winner", 100.0);
    raw_score_winner.score_kind = RuntimeChunkScoreKind::Relevance;
    let raw_score_winner_id = raw_score_winner.chunk_id;
    let mut semantic_winner = runtime_chunk("semantic winner", 0.1);
    semantic_winner.score_kind = RuntimeChunkScoreKind::Relevance;
    let semantic_winner_id = semantic_winner.chunk_id;
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![raw_score_winner, semantic_winner],
    };
    let semantic_ranks = HashMap::from([(semantic_winner_id, 0), (raw_score_winner_id, 1)]);

    truncate_bundle_with_semantic_chunk_ranks(
        &mut bundle,
        1,
        None,
        &HashSet::new(),
        &semantic_ranks,
    );

    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(bundle.chunks[0].chunk_id, semantic_winner_id);
    assert_eq!(
        bundle.chunks[0].score,
        Some(0.1),
        "semantic rank must not rewrite provenance score"
    );
}

#[test]
fn truncate_bundle_keeps_safety_lane_priority_ahead_of_semantic_rank() {
    let mut graph_evidence = runtime_chunk("mandatory graph evidence", 0.1);
    graph_evidence.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    let graph_evidence_id = graph_evidence.chunk_id;
    let mut semantic_winner = runtime_chunk("ordinary semantic winner", 100.0);
    semantic_winner.score_kind = RuntimeChunkScoreKind::Relevance;
    let semantic_winner_id = semantic_winner.chunk_id;
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![semantic_winner, graph_evidence],
    };
    let semantic_ranks = HashMap::from([(semantic_winner_id, 0)]);

    truncate_bundle_with_semantic_chunk_ranks(
        &mut bundle,
        1,
        None,
        &HashSet::new(),
        &semantic_ranks,
    );

    assert_eq!(bundle.chunks[0].chunk_id, graph_evidence_id);
}

#[test]
fn truncate_bundle_demotes_attached_context_documents_below_peers() {
    let attached_doc = Uuid::now_v7();
    let primary_doc = Uuid::now_v7();
    // The attached-context chunk has a far HIGHER raw score than the primary
    // chunk, so absent demotion it would win the single context slot — exactly
    // the OCR-screenshot-floods-the-answer failure this guards against.
    let attached = runtime_chunk_for_document(attached_doc, "screenshot", 10_000.0);
    let primary = runtime_chunk_for_document(primary_doc, "procedure", 1.0);
    let attached_id = attached.chunk_id;
    let primary_id = primary.chunk_id;

    // Baseline (no demotion): the high-scored attached-context chunk wins.
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![attached.clone(), primary.clone()],
    };
    truncate_bundle(&mut bundle, 1, None, &std::collections::HashSet::new());
    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(bundle.chunks[0].chunk_id, attached_id);

    // With the attached-context document demoted, the lower-scored primary
    // chunk takes the slot instead.
    let mut demoted = std::collections::HashSet::new();
    demoted.insert(attached_doc);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![attached, primary],
    };
    truncate_bundle(&mut bundle, 1, None, &demoted);
    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(bundle.chunks[0].chunk_id, primary_id);
}

#[test]
fn truncate_bundle_reserves_source_context_for_exact_technical_queries() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut source_context = runtime_chunk("owner = 100,200\n100 = first\n200 = second", 1.0);
    source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
    source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 1.0);
    let source_context_id = source_context.chunk_id;
    chunks.push(source_context);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&exact_error_code_query_ir()),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), 4);
    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == source_context_id));
}

#[test]
fn truncate_bundle_caps_error_code_source_context_reservation() {
    let mut graph_evidence = runtime_chunk("high confidence graph evidence", 1.0);
    graph_evidence.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    graph_evidence.score = Some(graph_evidence_chunk_score(0));
    let graph_evidence_id = graph_evidence.chunk_id;

    let mut relevance = runtime_chunk("ordinary relevance", 10_000.0);
    relevance.score_kind = RuntimeChunkScoreKind::Relevance;

    let mut chunks = vec![graph_evidence, relevance];
    for index in 0..4 {
        let mut source_context =
            runtime_chunk(&format!("owner{index} = 100,200\n100 = first\n200 = second"), 1.0);
        source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
        source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 50.0 - index as f32);
        chunks.push(source_context);
    }
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&exact_error_code_query_ir()),
        &std::collections::HashSet::new(),
    );

    let source_context_count = bundle
        .chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    assert_eq!(source_context_count, 2);
    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == graph_evidence_id));
}

fn configure_how_focus_query_ir(focus: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Procedure],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint { hint: focus.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

#[test]
fn truncate_bundle_reserves_setup_focus_anchor_for_configure_how() {
    // High-scored filler chunks from the focused document would fill the whole
    // top_k by score, pushing the low-scored install anchor out. The reserved
    // slot must keep the anchor (package command + config path) in context.
    let mut chunks = (0..8)
        .map(|index| runtime_chunk("Sample Subject admin guide", 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut anchor = runtime_chunk("Sample Subject admin guide", 1.0);
    anchor.source_text =
        "Module configuration\nsample-runner --install sample-link\nSettings in the file /opt/alpha/connector/connector.conf".to_string();
    let anchor_id = anchor.chunk_id;
    chunks.push(anchor);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&configure_how_focus_query_ir("Sample Subject")),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), 4);
    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == anchor_id),
        "focused-document setup anchor must survive truncation"
    );
}

#[test]
fn truncate_bundle_reserves_versioned_update_procedure_body() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused_noise = runtime_chunk("Sample Target overview", 9_000.0);
    focused_noise.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    chunks.push(focused_noise);
    let mut procedure = runtime_chunk("Sample Target update guide", 1.0);
    procedure.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    procedure.document_label = "Sample Target update guide".to_string();
    procedure.source_text = "Procedure\n\
        1. Remove /etc/alpha/sources.list.\n\
        2. Install alpha-upgrade package.\n\
        3. Run ./upgrade_alpha.sh."
        .to_string();
    let procedure_id = procedure.chunk_id;
    chunks.push(procedure);
    let mut query_ir = target_entities_query_ir(&["Sample Target"]);
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Procedure,
        crate::domains::query_ir::QueryTargetKind::Version,
    ];
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == procedure_id),
        "versioned update body chunk must survive final context truncation"
    );
}

#[test]
fn truncate_bundle_prefers_exact_versioned_update_runbook_over_short_title_anchors() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 500.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();

    for (label, score) in
        [("Sample Target update note", 10_000.0), ("Sample Target update checklist", 9_999.0)]
    {
        let mut short_anchor = runtime_chunk(label, score);
        short_anchor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        short_anchor.source_text = format!(
            "{label}\n\
             1. Check the current Sample Target version.\n\
             2. Read the release note.\n\
             3. Run sample-runner --refresh."
        );
        chunks.push(short_anchor);
    }

    let mut runbook = runtime_chunk("Sample Target update runbook", 1.0);
    runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    runbook.source_text = "Sample Target update runbook\n\
         1. Run sample-runner --refresh.\n\
         2. Run sample-runner --apply.\n\
         3. Run sudo sample-configure alpha-rest.\n\
         4. Run sudo service alpha-rest restart."
        .to_string();
    let runbook_id = runbook.chunk_id;
    chunks.push(runbook);

    let mut query_ir = target_entities_query_ir(&["Sample Target"]);
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Procedure,
        crate::domains::query_ir::QueryTargetKind::Version,
    ];
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == runbook_id),
        "exact-target command runbook must outrank shorter exact-title anchors: {:#?}",
        bundle
            .chunks
            .iter()
            .map(|chunk| (&chunk.document_label, &chunk.source_text))
            .collect::<Vec<_>>()
    );
}

#[test]
fn truncate_bundle_does_not_reserve_acronym_collision_without_full_subject_body() {
    let collision_document_id = Uuid::now_v7();
    let mut collision_identity =
        runtime_chunk_for_document(collision_document_id, "SCS update guide", 1.0);
    collision_identity.source_text =
        "SCS is the short name of an unrelated sampling component.".to_string();
    collision_identity.excerpt = collision_identity.source_text.clone();
    let mut collision_procedure =
        runtime_chunk_for_document(collision_document_id, "SCS update guide", 0.5);
    collision_procedure.chunk_index = 1;
    collision_procedure.source_text = concat!(
        "SCS update procedure\n",
        "1. Back up the sampling component.\n",
        "2. Run unrelatedctl refresh.\n",
        "3. Restart the sampling component."
    )
    .to_string();
    collision_procedure.excerpt = collision_procedure.source_text.clone();

    let mut chunks = (0..13)
        .map(|index| runtime_chunk(&format!("Noise Suite {index}"), 50_000.0 - index as f32))
        .collect::<Vec<_>>();
    chunks.extend([collision_identity, collision_procedure]);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };
    let mut query_ir = configure_how_focus_query_ir("Sample Control Service");
    query_ir.target_entities = vec![EntityMention {
        label: "Sample Control Service".to_string(),
        role: EntityRole::Subject,
    }];
    query_ir.retrieval_query = Some("how to update Sample Control Service?".to_string());

    truncate_bundle(&mut bundle, 12, Some(&query_ir), &HashSet::new());

    assert!(
        bundle.chunks.iter().all(|chunk| chunk.document_id != collision_document_id),
        "an acronym collision without the full subject in the same document must not receive a reserved slot"
    );
}

#[test]
fn truncate_bundle_does_not_reserve_anchor_without_document_focus() {
    // Same chunks, but the query has no document_focus → no reservation, the
    // low-scored anchor is truncated like any other chunk.
    let mut chunks = (0..8)
        .map(|index| runtime_chunk("Sample Subject admin guide", 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut anchor = runtime_chunk("Sample Subject admin guide", 1.0);
    anchor.source_text =
        "sample-runner --install sample-link\nSettings in the file /opt/alpha/connector/connector.conf"
            .to_string();
    let anchor_id = anchor.chunk_id;
    chunks.push(anchor);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    let mut query_ir = configure_how_focus_query_ir("Sample Subject");
    query_ir.document_focus = None;
    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 4);
    assert!(!bundle.chunks.iter().any(|chunk| chunk.chunk_id == anchor_id));
}

#[test]
fn truncate_bundle_reserves_source_context_for_transport_inventory_queries() {
    let mut chunks = (0..12)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut source_context_ids = Vec::new();
    for index in 0..8 {
        let mut source_context = runtime_chunk(
            &format!("serviceUrl{index} = http://localhost:8080\nservicePort{index} = 8080"),
            1.0,
        );
        source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
        source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 8.0 - index as f32);
        source_context_ids.push(source_context.chunk_id);
        chunks.push(source_context);
    }
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        10,
        Some(&transport_inventory_query_ir()),
        &std::collections::HashSet::new(),
    );

    let source_context_count = bundle
        .chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    assert_eq!(source_context_count, 8);
    assert!(
        source_context_ids.iter().all(|id| bundle.chunks.iter().any(|chunk| chunk.chunk_id == *id))
    );
}
