use super::*;

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
