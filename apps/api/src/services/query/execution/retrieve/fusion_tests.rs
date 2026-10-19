use super::test_prelude::*;

pub(super) fn runtime_chunk(label: &str, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: label.to_string(),
        excerpt: label.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: label.to_string(),
    }
}

pub(super) fn runtime_chunk_for_document(
    document_id: Uuid,
    label: &str,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk { document_id, ..runtime_chunk(label, score) }
}

pub(super) fn latest_version_identity_chunk(
    document_id: Uuid,
    requested_count: usize,
    document_rank: usize,
    chunk_rank: usize,
) -> RuntimeMatchedChunk {
    let score = latest_version_chunk_score(
        DOCUMENT_IDENTITY_SCORE_FLOOR,
        requested_count,
        document_rank,
        chunk_rank,
    );
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: chunk_rank as i32,
        chunk_kind: Some("paragraph".to_string()),
        document_id,
        document_label: format!("Release {document_rank}"),
        excerpt: format!("release {document_rank} chunk {chunk_rank}"),
        score_kind: RuntimeChunkScoreKind::LatestVersion,
        score: Some(score),
        source_text: format!("release {document_rank} chunk {chunk_rank}"),
    }
}

#[test]
fn latest_version_document_scope_drops_non_release_tail() {
    let release_a = Uuid::now_v7();
    let release_b = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let mut chunks = vec![
        runtime_chunk_for_document(release_a, "Version 9.8.765 Product Notes", 10.0),
        runtime_chunk_for_document(release_b, "Version 9.8.764 Product Notes", 9.0),
        runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0),
    ];

    retain_scoped_documents(
        &mut chunks,
        &BTreeSet::new(),
        &BTreeSet::from([release_a, release_b]),
        &BTreeSet::new(),
    );

    assert_eq!(chunks.len(), 2);
    assert!(chunks.iter().all(|chunk| chunk.document_id != generic));
}

#[test]
fn latest_version_document_scope_falls_back_when_no_release_documents() {
    let generic = Uuid::now_v7();
    let mut chunks = vec![runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0)];

    retain_scoped_documents(&mut chunks, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new());

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].document_id, generic);
}

#[test]
fn latest_version_document_scope_unions_with_targeted_documents() {
    let release = Uuid::now_v7();
    let targeted = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let mut chunks = vec![
        runtime_chunk_for_document(release, "Version 9.8.765 Product Notes", 10.0),
        runtime_chunk_for_document(targeted, "Explicitly Targeted Guide", 9.0),
        runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0),
    ];

    retain_scoped_documents(
        &mut chunks,
        &BTreeSet::from([targeted]),
        &BTreeSet::from([release]),
        &BTreeSet::new(),
    );

    let retained = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    assert_eq!(retained, BTreeSet::from([release, targeted]));
}

#[test]
fn merge_chunks_preserves_identity_scale_scores() {
    let ordinary = runtime_chunk("ordinary", 10.0);
    let identity = runtime_chunk("identity", DOCUMENT_IDENTITY_SCORE_FLOOR);

    let merged = merge_chunks(vec![ordinary], vec![identity.clone()], 8);

    assert_eq!(merged[0].chunk_id, identity.chunk_id);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn entity_bio_chunks_use_explicit_merge_lane_priority() {
    let ordinary = runtime_chunk("ordinary", 10_000.0);
    let entity_bio = runtime_chunk("entity bio", entity_bio_chunk_score(0));

    let merged = merge_entity_bio_chunks(vec![ordinary], vec![entity_bio.clone()], 8);

    assert_eq!(merged[0].chunk_id, entity_bio.chunk_id);
    assert_eq!(merged[0].score, Some(entity_bio_chunk_score(0)));
    assert!(entity_bio_chunk_score(0) < DOCUMENT_IDENTITY_SCORE_FLOOR);
}

#[test]
fn entity_bio_uses_every_typed_nonempty_entity_without_casing_routing() {
    let query_ir = target_entities_query_ir(&["Alpha Node", "lowercase node", "节点"]);

    let labels = entity_bio_target_mentions(&query_ir)
        .into_iter()
        .map(|mention| mention.label.as_str())
        .collect::<Vec<_>>();

    assert_eq!(labels, ["Alpha Node", "lowercase node", "节点"]);
}

#[test]
fn graph_evidence_chunks_use_explicit_merge_lane_priority() {
    let ordinary = runtime_chunk("ordinary", 10_000.0);
    let graph_evidence = runtime_chunk("graph evidence", graph_evidence_chunk_score(0));

    let merged = merge_graph_evidence_chunks(vec![ordinary], vec![graph_evidence.clone()], 8);

    assert_eq!(merged[0].chunk_id, graph_evidence.chunk_id);
    assert_eq!(merged[0].score, Some(graph_evidence_chunk_score(0)));
}

#[test]
fn graph_evidence_promotes_for_structural_relationship_ir() {
    let mut query_ir = target_entities_query_ir(&["Alpha Node", "Beta Endpoint"]);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Relationship];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_relation_like_event_ir() {
    let query_ir = target_entities_query_ir(&["Beacon Node", "Harbor Delta"]);

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_artifact_selection_ir() {
    let mut query_ir = target_entities_query_ir(&["Router Hub"]);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Artifact];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_does_not_promote_for_single_document_concept_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];

    assert!(!query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_fact_value_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_fact_inventory_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_does_not_promote_for_exact_technical_ir() {
    let mut query_ir = target_entities_query_ir(&["Alpha Node", "Beta Endpoint"]);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Relationship];
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "/v1/alpha".to_string(), kind: LiteralKind::Path }];

    assert!(!query_ir_promotes_graph_evidence(&query_ir));
}
