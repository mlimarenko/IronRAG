use super::test_prelude::*;

#[test]
fn truncate_bundle_expands_entity_budget_for_inventory_queries() {
    let entities = (0..12)
        .map(|index| RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: format!("Package {index:02}"),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(1.0),
        })
        .collect::<Vec<_>>();
    let chunks =
        (0..12).map(|index| runtime_chunk(&format!("chunk-{index:02}"), 1.0)).collect::<Vec<_>>();
    let mut bundle = RetrievalBundle { entities, relationships: Vec::new(), chunks };
    let query_ir = release_query_ir(None, Some("package-*"));

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.entities.len(), 12);
    assert_eq!(bundle.chunks.len(), 4);
}

#[test]
fn truncate_bundle_keeps_requested_latest_version_document_coverage() {
    let requested_count = 10;
    let document_ids = (0..requested_count).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let mut chunks = Vec::new();
    for chunk_rank in 0..2 {
        for (document_rank, document_id) in document_ids.iter().copied().enumerate() {
            chunks.push(latest_version_identity_chunk(
                document_id,
                requested_count,
                document_rank,
                chunk_rank,
            ));
        }
    }
    let mut query_ir = release_query_ir_with_source_slice_count(Some(requested_count as u16));
    query_ir.scope = QueryScope::SingleDocument;
    let effective_top_k = source_slice_context_top_k(&query_ir, 8);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        effective_top_k,
        Some(&query_ir),
        &std::collections::HashSet::new(),
    );

    let retained_documents =
        bundle.chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    assert_eq!(effective_top_k, 11);
    assert_eq!(retained_documents.len(), requested_count);
}

#[test]
fn truncate_bundle_reserves_latest_version_chunks_before_stale_document_identity_hits() {
    let requested_count = 5;
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk =
                runtime_chunk(&format!("latest releases stale context {index}"), 1_000_000.0);
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    chunks.extend((0..requested_count).map(|document_rank| {
        latest_version_identity_chunk(Uuid::now_v7(), requested_count, document_rank, 0)
    }));
    let mut query_ir = release_query_ir_with_source_slice_count(Some(requested_count as u16));
    query_ir.scope = QueryScope::MultiDocument;
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        requested_count,
        Some(&query_ir),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), requested_count);
    assert_eq!(
        bundle
            .chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
            .count(),
        requested_count
    );
}

#[test]
fn truncate_bundle_preserves_multi_document_relevant_coverage_for_compare_queries() {
    let rust_document = Uuid::now_v7();
    let llm_document = Uuid::now_v7();
    let rust_chunk = |score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: rust_document,
        document_label: "rust_programming_language_wikipedia".to_string(),
        excerpt: "Rust language runtime context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "Rust language context".to_string(),
    };
    let llm_chunk = |score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: llm_document,
        document_label: "large_language_model_wikipedia".to_string(),
        excerpt: "LLM context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "Large Language Model context".to_string(),
    };

    let mut query_ir = target_entities_query_ir(&["Rust", "Large Language Model"]);
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.comparison = Some(ComparisonSpec {
        a: Some("Rust".to_string()),
        b: Some("Large Language Model".to_string()),
        dimension: "features".to_string(),
    });

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            rust_chunk(10.0),
            rust_chunk(9.9),
            rust_chunk(9.8),
            rust_chunk(9.7),
            rust_chunk(9.6),
            rust_chunk(9.5),
            llm_chunk(8.0),
            llm_chunk(7.9),
        ],
    };

    truncate_bundle(&mut bundle, 6, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 6);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == rust_document));
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == llm_document));
}

#[test]
fn truncate_bundle_ranks_structurally_named_comparison_document_first() {
    const RESULT_LIMIT: usize = 3;

    let topic_neighbor_id = Uuid::now_v7();
    let first_target_id = Uuid::now_v7();
    let second_target_id = Uuid::now_v7();
    let third_target_id = Uuid::now_v7();
    let mut topic_neighbor =
        runtime_chunk_for_document(topic_neighbor_id, "failure_behavior_reference.md", 100.0);
    topic_neighbor.source_text =
        "Failure behavior patterns for Alpha State Engine, Beta Data Flow, and Gamma HTTP Service"
            .to_string();
    let first_target = runtime_chunk_for_document(first_target_id, "alpha_state_engine.rs", 1.0);
    let second_target = runtime_chunk_for_document(second_target_id, "beta_data_flow.py", 0.9);
    let third_target = runtime_chunk_for_document(third_target_id, "gamma_http_service.yaml", 0.8);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![topic_neighbor, first_target, second_target, third_target],
    };
    let target_ids = [first_target_id, second_target_id, third_target_id];
    let query_ir =
        comparison_query_ir(&["Alpha State Engine", "Beta Data Flow", "Gamma HTTP Service"]);

    truncate_bundle(&mut bundle, RESULT_LIMIT, Some(&query_ir), &std::collections::HashSet::new());

    assert!(
        target_ids.contains(&bundle.chunks[0].document_id),
        "one of the structurally named documents must lead: {:#?}",
        bundle.chunks
    );
    assert!(
        target_ids
            .iter()
            .all(|target_id| bundle.chunks.iter().any(|chunk| chunk.document_id == *target_id)),
        "all structurally named documents must survive truncation: {:#?}",
        bundle.chunks
    );
}

#[test]
fn truncate_bundle_keeps_single_subject_ranking_unchanged() {
    const RESULT_LIMIT: usize = 1;

    let topic_winner_id = Uuid::now_v7();
    let named_subject_id = Uuid::now_v7();
    let topic_winner = runtime_chunk_for_document(topic_winner_id, "failure_reference.md", 100.0);
    let named_subject = runtime_chunk_for_document(named_subject_id, "alpha_state_engine.rs", 1.0);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![topic_winner, named_subject],
    };
    let query_ir = target_entities_query_ir(&["Alpha State Engine"]);

    truncate_bundle(&mut bundle, RESULT_LIMIT, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks[0].document_id, topic_winner_id);
}

#[test]
fn truncate_bundle_does_not_promote_body_only_match_for_missing_comparison_subjects() {
    const RESULT_LIMIT: usize = 1;

    let topic_winner_id = Uuid::now_v7();
    let lower_ranked_id = Uuid::now_v7();
    let mut topic_winner =
        runtime_chunk_for_document(topic_winner_id, "failure_reference.md", 100.0);
    topic_winner.source_text =
        "Missing State Engine and Absent Data Flow failure behavior".to_string();
    let lower_ranked = runtime_chunk_for_document(lower_ranked_id, "state_notes.md", 1.0);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![topic_winner, lower_ranked],
    };
    let query_ir = comparison_query_ir(&["Missing State Engine", "Absent Data Flow"]);

    truncate_bundle(&mut bundle, RESULT_LIMIT, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks[0].document_id, topic_winner_id);
}

#[test]
fn truncate_bundle_compare_fallback_keeps_second_document_when_terms_are_not_present() {
    let first_document = Uuid::now_v7();
    let second_document = Uuid::now_v7();
    let chunk = |document_id: Uuid, label: &str, score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: label.to_string(),
        excerpt: "neutral context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "neutral context".to_string(),
    };

    let mut query_ir = target_entities_query_ir(&["Alpha", "Beta"]);
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.comparison = Some(ComparisonSpec {
        a: Some("Alpha".to_string()),
        b: Some("Beta".to_string()),
        dimension: "difference".to_string(),
    });

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            chunk(first_document, "first.md", 10.0),
            chunk(first_document, "first.md", 9.9),
            chunk(first_document, "first.md", 9.8),
            chunk(second_document, "second.md", 8.0),
        ],
    };

    truncate_bundle(&mut bundle, 2, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 2);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == first_document));
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == second_document));
}

#[test]
fn graph_evidence_merge_preserves_prior_query_ir_focus_score_kind() {
    let ordinary = runtime_chunk("ordinary", 0.02);
    let graph_evidence = runtime_chunk("graph evidence", graph_evidence_chunk_score(0));
    let exact_focus = runtime_chunk("exact focus", 100.0);

    let merged = merge_query_ir_focus_chunks(vec![ordinary], vec![exact_focus.clone()], 8);
    let merged = merge_graph_evidence_chunks(merged, vec![graph_evidence], 8);

    assert_eq!(merged[0].chunk_id, exact_focus.chunk_id);
    assert_eq!(merged[0].score_kind, RuntimeChunkScoreKind::QueryIrFocus);
    assert_eq!(merged[0].score, Some(100.0));
}

#[test]
fn entity_bio_lane_does_not_override_document_identity_priority() {
    let identity = runtime_chunk("identity", DOCUMENT_IDENTITY_SCORE_FLOOR);
    let entity_bio = runtime_chunk("entity bio", entity_bio_chunk_score(0));

    let merged = merge_entity_bio_chunks(vec![identity.clone()], vec![entity_bio], 8);

    assert_eq!(merged[0].chunk_id, identity.chunk_id);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn rrf_merge_preserves_latest_version_score_kind() {
    let mut latest = runtime_chunk("latest", DOCUMENT_IDENTITY_SCORE_FLOOR);
    latest.score_kind = RuntimeChunkScoreKind::LatestVersion;
    let ordinary = runtime_chunk("ordinary", 0.02);

    let merged = merge_chunks(vec![ordinary], vec![latest.clone()], 8);

    assert_eq!(merged[0].chunk_id, latest.chunk_id);
    assert_eq!(merged[0].score_kind, RuntimeChunkScoreKind::LatestVersion);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn entity_bio_scores_keep_fanout_order_inside_lane() {
    assert!(entity_bio_chunk_score(0) > entity_bio_chunk_score(1));
    assert!(entity_bio_chunk_score(23) > 0.9);
}

#[test]
fn graph_evidence_scores_keep_fanout_order_inside_entity_bio_lane() {
    assert!(graph_evidence_chunk_score(0) > graph_evidence_chunk_score(1));
    assert!(graph_evidence_chunk_score(0) > entity_bio_chunk_score(0));
    assert!(graph_evidence_chunk_score(23) < DOCUMENT_IDENTITY_SCORE_FLOOR);
}
