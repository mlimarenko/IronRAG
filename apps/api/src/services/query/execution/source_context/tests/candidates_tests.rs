use super::*;
use crate::services::query::assistant_prompt::render_single_shot;
use crate::services::query::execution::{
    AuthoritativeAnswerEvidence, RetrievalBundle, render_primary_chunk_section,
    retrieve::truncate_bundle_with_semantic_chunk_ranks,
    semantic_verification::{EvidenceBudget, measure_selected_evidence, retain_bounded_chunks},
};

#[test]
fn overview_budget_keeps_the_definition_peer_and_ranked_update() {
    let record = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let peer = document_row(Uuid::from_u128(81), Uuid::from_u128(82));
    let passage = |doc: &KnowledgeDocumentRow, ordinal, text: String| RuntimeMatchedChunk {
        source_text: text.clone(),
        excerpt: text,
        chunk_kind: Some("paragraph".into()),
        ..runtime_chunk(doc.document_id, doc.readable_revision_id.unwrap(), ordinal)
    };
    let update = "Release check is still pending. ".repeat(166);
    let mut chunks =
        (20..40).map(|rank| passage(&record, rank, update.clone())).collect::<Vec<_>>();
    let mut definition = passage(&record, 1, "Purpose: support independent review. ".repeat(40));
    definition.score = Some(-2.0);
    let handoff = passage(&peer, 80, "The archived plan used a manual handoff. ".repeat(40));
    let ranked_update = chunks[0].clone();
    chunks.extend([definition.clone(), handoff.clone()]);
    let documents = HashMap::from([(record.document_id, record), (peer.document_id, peer)]);
    let mut ir = source_context_ir(QueryAct::Describe, QueryScope::MultiDocument);
    ir.literal_constraints.push(LiteralSpan { kind: LiteralKind::Version, text: "v2".into() });
    let mut lookup_ir = ir.clone();
    lookup_ir.act = QueryAct::RetrieveValue;
    let mut lookup = chunks.clone();
    retain_bounded_chunks(&mut lookup, &mut EvidenceBudget::new(), &lookup_ir, &documents);
    assert!(
        lookup
            .iter()
            .all(|chunk| chunk.document_id == ranked_update.document_id && chunk.chunk_index >= 20)
    );

    let retention = retain_bounded_chunks(&mut chunks, &mut EvidenceBudget::new(), &ir, &documents);
    assert!(retention.is_truncated());
    for required in [&ranked_update, &definition, &handoff] {
        assert!(chunks.iter().any(|source| source.chunk_id == required.chunk_id
            && source.source_text == required.source_text));
    }
    assert_eq!(chunks[0].chunk_id, ranked_update.chunk_id);
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: vec![],
        structured_blocks: vec![],
        technical_facts: vec![],
    };
    let checked = measure_selected_evidence(&chunks, &evidence, &Default::default())
        .expect("admitted evidence fits");
    assert_eq!(checked.measurement, retention.measurement);
    let mut bundle = RetrievalBundle { chunks, entities: vec![], relationships: vec![] };
    truncate_bundle_with_semantic_chunk_ranks(
        &mut bundle,
        3,
        Some(&ir),
        &Default::default(),
        &Default::default(),
        &documents,
    );
    assert_eq!(bundle.chunks.len(), 3);
    let prompt = render_primary_chunk_section(
        "Describe release v2 and the older plan.",
        &ir,
        &bundle.chunks,
        false,
    );
    for excerpt in ["independent review", "manual handoff", "still pending"] {
        assert!(prompt.contains(excerpt), "{prompt}");
    }
}

#[test]
fn tight_overview_budget_keeps_the_multi_section_opening_and_marks_the_sample_limit() {
    let record = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let document_id = record.document_id;
    let revision_id = record.readable_revision_id.unwrap();
    let documents = HashMap::from([(document_id, record)]);
    let passage = |index: i32, kind: &str, text: String, score: f32| RuntimeMatchedChunk {
        source_text: text.clone(),
        excerpt: text,
        chunk_kind: Some(kind.into()),
        score: Some(score),
        ..runtime_chunk(document_id, revision_id, index)
    };
    let definition = "Definition: one supported system without an active legacy path. ".repeat(12);
    let acceptance =
        "Acceptance: install through the UI, negative permission checks, exact build checks. "
            .repeat(12);
    let progress =
        "2026-11-13 progress: the migration is still pending on this record. ".repeat(10);
    let mut chunks = vec![
        passage(0, "heading", definition.clone(), 0.1),
        passage(1, "heading", acceptance.clone(), 0.2),
    ];
    chunks.extend((2..96).map(|index| {
        passage(
            index,
            "heading",
            format!("{progress}record {index}"),
            1.0 + f32::from(index as u16) * 0.001,
        )
    }));

    let ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    let mut retained = chunks.clone();
    let retention =
        retain_bounded_chunks(&mut retained, &mut EvidenceBudget::new(), &ir, &documents);
    assert!(retention.is_truncated(), "the stored revision exceeds one bounded evidence budget");
    for opening in [0, 1] {
        assert!(
            retained.iter().any(|chunk| chunk.chunk_index == opening),
            "opening section {opening} must survive the evidence budget"
        );
    }

    let mut bundle = RetrievalBundle { chunks, entities: vec![], relationships: vec![] };
    truncate_bundle_with_semantic_chunk_ranks(
        &mut bundle,
        4,
        Some(&ir),
        &Default::default(),
        &Default::default(),
        &documents,
    );
    assert_eq!(bundle.chunks.len(), 4);
    for opening in [0, 1] {
        assert!(
            bundle.chunks.iter().any(|chunk| chunk.chunk_index == opening),
            "opening section {opening} must survive a tight top-K through redundant slots"
        );
    }

    retain_bounded_chunks(&mut bundle.chunks, &mut EvidenceBudget::new(), &ir, &documents);
    for opening in [0, 1] {
        assert!(
            bundle.chunks.iter().any(|chunk| chunk.chunk_index == opening),
            "opening section {opening} must survive admission of the truncated bundle"
        );
    }

    let context = render_primary_chunk_section(
        "Describe the supported system and how acceptance is proven.",
        &ir,
        &bundle.chunks,
        false,
    );
    for excerpt in ["one supported system", "negative permission checks", "still pending"] {
        assert!(context.contains(excerpt), "{context}");
    }
    assert!(context.contains("coverage=sampled; recency=uncertified"), "{context}");
    assert!(context.contains("completeness=uncertified"), "{context}");

    let system_prompt = render_single_shot(QueryLanguage::Ru);
    assert!(system_prompt.contains("recency=uncertified"), "{system_prompt}");
    assert!(system_prompt.contains("historical evidence for its own date"), "{system_prompt}");

    // The per-document render cap still spends spare slots on both opening
    // sections when the admitted set is larger than the sample.
    let render_budget_context = render_primary_chunk_section(
        "Describe the recorded progress on the migration.",
        &ir,
        &retained,
        false,
    );
    assert!(render_budget_context.contains("one supported system"), "{render_budget_context}");
    assert!(
        render_budget_context.contains("negative permission checks"),
        "{render_budget_context}"
    );
    assert!(render_budget_context.contains("still pending"), "{render_budget_context}");

    // A two-slot render sample still spends its spare slot on the primary
    // opening instead of replacing the document's sole best match.
    let render_ir = source_context_ir(QueryAct::Compare, QueryScope::SingleDocument);
    let narrow_context = render_primary_chunk_section(
        "Compare the recorded progress on the migration.",
        &render_ir,
        &retained,
        false,
    );
    assert!(narrow_context.contains("one supported system"), "{narrow_context}");
    assert!(narrow_context.contains("still pending"), "{narrow_context}");
    assert!(narrow_context.contains("coverage=sampled; recency=uncertified"), "{narrow_context}");
}

#[test]
fn single_opening_slot_keeps_the_head_policy_over_reverse_ranked_adjacent() {
    let record = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let document_id = record.document_id;
    let revision_id = record.readable_revision_id.unwrap();
    let passage = |index: i32, text: &str, score: f32| RuntimeMatchedChunk {
        source_text: text.to_string(),
        excerpt: text.to_string(),
        chunk_kind: Some("paragraph".into()),
        score: Some(score),
        ..runtime_chunk(document_id, revision_id, index)
    };
    let head = "Definition: one supported system without an active legacy path.";
    let adjacent = "Acceptance: negative permission checks and exact build checks.";
    // Retrieval position ranks the adjacent chunk above the source head.
    let chunks = vec![
        passage(2, "Progress update on the migration.", 10.0),
        passage(3, "Second progress update on the migration.", 9.0),
        passage(1, adjacent, 0.4),
        passage(0, head, 0.5),
    ];
    let openings =
        document_overview_openings(&chunks[0], &chunks).expect("head window has two openings");
    assert_eq!(chunks[openings.primary].chunk_index, 0);
    assert_eq!(
        openings.reading_order.iter().map(|rank| chunks[*rank].chunk_index).collect::<Vec<_>>(),
        vec![0, 1]
    );

    let ir = source_context_ir(QueryAct::Compare, QueryScope::SingleDocument);
    let context =
        render_primary_chunk_section("Describe the deployment windows.", &ir, &chunks, false);
    assert!(context.contains(head), "{context}");
    assert!(!context.contains(adjacent), "{context}");

    let mut protected = chunks.clone();
    protected[1].score_kind =
        crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity;
    let context =
        render_primary_chunk_section("Describe the deployment windows.", &ir, &protected, false);
    assert!(context.contains(&protected[1].source_text), "{context}");
    assert!(!context.contains(head), "{context}");

    // A bare heading at the nominal head does not win a single slot over the
    // adjacent body chunk, while two slots keep source reading order.
    let heading = RuntimeMatchedChunk {
        chunk_kind: Some("heading".into()),
        source_text: "Section title".into(),
        excerpt: "Section title".into(),
        ..runtime_chunk(document_id, revision_id, 0)
    };
    let body = RuntimeMatchedChunk {
        chunk_kind: Some("paragraph".into()),
        source_text: "Substantive opening body.".into(),
        excerpt: "Substantive opening body.".into(),
        ..runtime_chunk(document_id, revision_id, 1)
    };
    let heading_body = [body, heading];
    let openings =
        document_overview_openings(&heading_body[0], &heading_body).expect("heading and body");
    assert_eq!(heading_body[openings.primary].chunk_index, 1);
    assert_eq!(
        openings
            .reading_order
            .iter()
            .map(|rank| heading_body[*rank].chunk_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn opening_sample_keeps_selected_head_and_fills_the_missing_adjacent() {
    let record = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let document_id = record.document_id;
    let revision_id = record.readable_revision_id.unwrap();
    let passage = |index: i32, text: &str, score: f32| RuntimeMatchedChunk {
        source_text: text.to_string(),
        excerpt: text.to_string(),
        chunk_kind: Some("paragraph".into()),
        score: Some(score),
        ..runtime_chunk(document_id, revision_id, index)
    };
    let head = "Head definition: one supported system without an active legacy path.";
    let adjacent = "Adjacent acceptance: negative permission checks and exact build checks.";
    let mut chunks = vec![
        passage(0, head, 9.0),
        passage(1, adjacent, 0.0),
        passage(2, "Progress update on the migration.", 10.0),
    ];
    chunks.extend((3..=24).map(|index| passage(index, "Progress update on the migration.", 1.0)));

    let ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    let context =
        render_primary_chunk_section("Describe the deployment windows.", &ir, &chunks, false);
    assert!(context.contains(head), "{context}");
    assert!(context.contains(adjacent), "{context}");
    assert!(context.contains("Progress update"), "{context}");
}

#[test]
fn version_qualified_describe_overview_uses_an_unused_total_slot_for_the_adjacent_opening() {
    let primary = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let peer_a = document_row(Uuid::from_u128(20), Uuid::from_u128(21));
    let peer_b = document_row(Uuid::from_u128(30), Uuid::from_u128(31));
    let passage =
        |document: &KnowledgeDocumentRow, index: i32, text: &str, score: f32| RuntimeMatchedChunk {
            source_text: text.to_string(),
            excerpt: text.to_string(),
            chunk_kind: Some("paragraph".into()),
            score: Some(score),
            ..runtime_chunk(document.document_id, document.readable_revision_id.unwrap(), index)
        };
    let head = "Definition: one supported system without an active legacy path.";
    let adjacent = "Acceptance: negative permission checks and exact build checks.";
    let chunks = vec![
        passage(&primary, 20, "Progress update on the migration.", 10.0),
        passage(&primary, 0, head, 9.0),
        passage(&primary, 1, adjacent, 8.0),
        passage(&peer_a, 0, "Unrelated peer alpha note.", 7.0),
        passage(&peer_b, 0, "Unrelated peer beta note.", 6.0),
    ];
    let mut ir = source_context_ir(QueryAct::Describe, QueryScope::MultiDocument);
    ir.literal_constraints.push(LiteralSpan { kind: LiteralKind::Version, text: "v2".into() });
    assert!(
        !ir.requests_source_coverage_context(),
        "a Version literal keeps the narrow two-chunk-per-document render cap"
    );
    assert!(ir.requests_document_overview_evidence());

    let context = render_primary_chunk_section(
        "Describe the deployment windows for v2.",
        &ir,
        &chunks,
        false,
    );

    assert!(context.contains(head), "{context}");
    assert!(context.contains(adjacent), "{context}");
    assert!(context.contains("Progress update"), "{context}");
    assert!(context.contains("Unrelated peer alpha note."), "{context}");
    assert!(context.contains("Unrelated peer beta note."), "{context}");
    assert_eq!(context.matches("- [EVIDENCE_CHUNK ").count(), 5, "{context}");
}

#[test]
fn short_updates_do_not_evict_the_opening_at_either_count_limit() {
    let doc = document_row(Uuid::from_u128(10), Uuid::from_u128(11));
    let document_id = doc.document_id;
    let revision_id = doc.readable_revision_id.unwrap();
    let documents = HashMap::from([(document_id, doc)]);
    let ir = source_context_ir(QueryAct::Compare, QueryScope::MultiDocument);
    let mut chunks: Vec<_> =
        (20..60).map(|i| companion_chunk(document_id, revision_id, i, "paragraph", 1.0)).collect();
    let mut opening = companion_chunk(document_id, revision_id, 0, "paragraph", 0.0);
    opening.source_text = "A load rating is a design limit, not a measured load.".into();
    opening.excerpt.clone_from(&opening.source_text);
    chunks.push(opening.clone());
    let selection = retain_bounded_chunks(&mut chunks, &mut EvidenceBudget::new(), &ir, &documents);
    assert!(!selection.is_truncated(), "the byte and entry budgets fit before top-K");
    let mut bundle = RetrievalBundle { entities: vec![], relationships: vec![], chunks };
    truncate_bundle_with_semantic_chunk_ranks(
        &mut bundle,
        30,
        Some(&ir),
        &Default::default(),
        &Default::default(),
        &documents,
    );
    assert_eq!(bundle.chunks.len(), 30);
    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == opening.chunk_id));
    let prompt =
        render_primary_chunk_section("Compare paragraph updates.", &ir, &bundle.chunks, false);
    assert!(prompt.contains(&opening.source_text), "{prompt}");
    assert!(prompt.contains("paragraph 20"), "{prompt}");
    assert_eq!(prompt.matches("- [EVIDENCE_CHUNK ").count(), 2, "{prompt}");
}

#[test]
fn candidate_revision_ids_keep_first_seen_order_and_drop_duplicates() {
    let revision_a = Uuid::from_u128(1);
    let revision_b = Uuid::from_u128(2);
    let document_a = Uuid::from_u128(10);
    let document_b = Uuid::from_u128(11);
    let document_c = Uuid::from_u128(12);
    let candidates = vec![
        SourceContextCandidate {
            document_id: document_a,
            revision_id: revision_a,
            first_rank: 0,
            best_score: 1.0,
            anchors: Vec::new(),
        },
        SourceContextCandidate {
            document_id: document_b,
            revision_id: revision_b,
            first_rank: 1,
            best_score: 0.9,
            anchors: Vec::new(),
        },
        SourceContextCandidate {
            document_id: document_c,
            revision_id: revision_a,
            first_rank: 2,
            best_score: 0.8,
            anchors: Vec::new(),
        },
    ];

    assert_eq!(unique_candidate_revision_ids(&candidates), vec![revision_a, revision_b]);
}

#[test]
fn profile_only_candidates_are_appended_under_separate_limit() {
    let profile_document_id = Uuid::from_u128(99);
    let profile_revision_id = Uuid::from_u128(199);
    let mut chunks = (0..4)
        .map(|index| {
            runtime_chunk(
                Uuid::from_u128(10 + u128::from(index as u32)),
                Uuid::from_u128(110 + u128::from(index as u32)),
                index,
            )
        })
        .collect::<Vec<_>>();
    chunks.push(companion_chunk(
        profile_document_id,
        profile_revision_id,
        0,
        "source_profile",
        12.0,
    ));

    let narrow = collect_source_context_candidates_with_limit(&chunks, 3, 0);
    assert_eq!(narrow.len(), 3);
    assert!(
        narrow.iter().all(|candidate| candidate.document_id != profile_document_id),
        "profile-only document must not appear when profile anchors are disabled"
    );

    let expanded = collect_source_context_candidates_with_limit(&chunks, 3, 2);
    assert_eq!(expanded.len(), 4);
    let profile_candidate = expanded
        .iter()
        .find(|candidate| candidate.document_id == profile_document_id)
        .expect("profile-only document appended");
    assert_eq!(profile_candidate.revision_id, profile_revision_id);
    assert_eq!(profile_candidate.anchors.len(), 1);
    assert_eq!(profile_candidate.anchors[0].chunk_index, 0);
}
#[test]
fn collect_candidates_limits_documents_and_anchors() {
    let revision_id = Uuid::now_v7();
    let docs = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let chunks = docs
        .iter()
        .flat_map(|document_id| {
            [
                runtime_chunk(*document_id, revision_id, 4),
                runtime_chunk(*document_id, revision_id, 5),
                runtime_chunk(*document_id, revision_id, 6),
            ]
        })
        .collect::<Vec<_>>();

    let candidates = collect_source_context_candidates(&chunks);

    assert_eq!(candidates.len(), SOURCE_CONTEXT_DOCUMENT_LIMIT);
    assert!(
        candidates.iter().all(|candidate| {
            candidate.anchors.len() == SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT
        })
    );
    assert_eq!(candidates[0].document_id, docs[0]);
}

#[test]
fn graph_evidence_documents_prepend_source_context_candidates() {
    let graph_document_id = Uuid::now_v7();
    let graph_revision_id = Uuid::now_v7();
    let generic_document_id = Uuid::now_v7();
    let generic_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([
        (graph_document_id, document_row(graph_document_id, graph_revision_id)),
        (generic_document_id, document_row(generic_document_id, generic_revision_id)),
    ]);
    let generic_chunk = RuntimeMatchedChunk {
        score: Some(4.0),
        ..runtime_chunk(generic_document_id, generic_revision_id, 7)
    };
    let candidates = collect_source_context_candidates(std::slice::from_ref(&generic_chunk));

    let merged = merge_graph_evidence_source_context_candidates(
        candidates,
        &[graph_document_id],
        &document_index,
        &[generic_chunk],
    );

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].document_id, graph_document_id);
    assert_eq!(merged[0].revision_id, graph_revision_id);
    assert!(merged[0].anchors.is_empty());
    assert_eq!(merged[1].document_id, generic_document_id);
}

#[test]
fn graph_evidence_documents_promote_existing_candidate_without_losing_anchors() {
    let graph_document_id = Uuid::now_v7();
    let graph_revision_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([
        (graph_document_id, document_row(graph_document_id, graph_revision_id)),
        (other_document_id, document_row(other_document_id, other_revision_id)),
    ]);
    let graph_chunk = RuntimeMatchedChunk {
        score: Some(1.0),
        ..runtime_chunk(graph_document_id, graph_revision_id, 3)
    };
    let other_chunk = RuntimeMatchedChunk {
        score: Some(5.0),
        ..runtime_chunk(other_document_id, other_revision_id, 4)
    };
    let candidates = collect_source_context_candidates(&[other_chunk.clone(), graph_chunk.clone()]);

    let merged = merge_graph_evidence_source_context_candidates(
        candidates,
        &[graph_document_id],
        &document_index,
        &[other_chunk, graph_chunk],
    );

    assert_eq!(merged[0].document_id, graph_document_id);
    assert_eq!(merged[0].anchors.len(), 1);
    assert_eq!(merged[0].anchors[0].chunk_index, 3);
    assert!(merged[0].best_score > 5.0);
}

#[test]
fn procedural_head_anchor_reaches_initial_setup_chunk() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let late_detail_chunk =
        RuntimeMatchedChunk { score: Some(7.0), ..runtime_chunk(document_id, revision_id, 22) };
    let mut candidates =
        collect_source_context_candidates(std::slice::from_ref(&late_detail_chunk));
    seed_document_head_source_context_anchors(&mut candidates);
    let span = SourceContextNeighborSpan {
        backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
        forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
    };

    assert_eq!(candidates.len(), 1);
    assert!(
        candidates[0]
            .anchors
            .iter()
            .any(|anchor| anchor.chunk_index == SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX)
    );
    assert!(source_context_neighbor_windows(&candidates[0].anchors, span).contains(
        &source_anchor_window(
            SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
            SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
        )
    ));
    assert!(
        source_context_best_neighbor_score(&candidates[0].anchors, 1, span).is_some(),
        "procedural source context must keep the document's setup block reachable"
    );
}

#[test]
fn collect_candidates_ranks_anchors_by_score_before_ordinal() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let low_ordinal_low_score = runtime_chunk(document_id, revision_id, 1);
    let high_ordinal_high_score =
        RuntimeMatchedChunk { score: Some(8.0), ..runtime_chunk(document_id, revision_id, 50) };
    let next_best =
        RuntimeMatchedChunk { score: Some(7.0), ..runtime_chunk(document_id, revision_id, 2) };

    let candidates = collect_source_context_candidates(&[
        low_ordinal_low_score,
        high_ordinal_high_score,
        next_best,
    ]);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].anchors[0].chunk_index, 50);
    assert_eq!(candidates[0].anchors[1].chunk_index, 2);
}

#[test]
fn source_profile_does_not_drive_candidate_score_or_anchors() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let profile = companion_chunk(document_id, revision_id, 0, "source_profile", 100.0);
    let content =
        RuntimeMatchedChunk { score: Some(3.0), ..runtime_chunk(document_id, revision_id, 8) };

    let candidates = collect_source_context_candidates(&[profile, content]);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].best_score, 3.0);
    assert_eq!(candidates[0].anchors.len(), 1);
    assert_eq!(candidates[0].anchors[0].chunk_index, 8);
}
