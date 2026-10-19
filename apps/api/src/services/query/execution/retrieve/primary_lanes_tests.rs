use super::test_prelude::*;

#[test]
fn vector_search_always_runs_regardless_of_exact_literal_flag() {
    // Required contract since v0.3.3: vector retrieval is always
    // exercised alongside lexical. The `exact_literal_technical` flag
    // on the intent profile influences ranking/boost, never excludes
    // the whole vector lane. Skipping the vector lane on exact-literal
    // questions causes BM25 stem collisions to promote unrelated
    // templates over the actual configuration sections.
    let mut literal_plan = RuntimeQueryPlan {
        requested_mode: crate::domains::query::RuntimeQueryMode::Document,
        planned_mode: crate::domains::query::RuntimeQueryMode::Document,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec!["endpoint".to_string()],
        high_level_keywords: vec!["endpoint".to_string()],
        low_level_keywords: vec!["system".to_string()],
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        top_k: 8,
        context_budget_chars: 4_000,
        hyde_recommended: false,
    };

    assert!(!should_skip_vector_search(&literal_plan));
    literal_plan.intent_profile.exact_literal_technical = true;
    assert!(!should_skip_vector_search(&literal_plan));
}

#[test]
fn chunk_retrieval_lanes_continue_when_vector_lane_fails() {
    let lexical_chunk = runtime_chunk("lexical", 0.8);

    let outcome = combine_chunk_retrieval_lanes(
        Err(anyhow::anyhow!("vector backend unavailable")),
        Ok((vec![lexical_chunk.clone()], 3, 42)),
    )
    .expect("lexical lane should keep retrieval usable");

    assert_eq!(outcome.degraded_lane_count, 1);
    assert!(outcome.vector_hits.is_empty());
    assert_eq!(outcome.lexical_hits.len(), 1);
    assert_eq!(outcome.lexical_hits[0].chunk_id, lexical_chunk.chunk_id);
    assert_eq!(outcome.lexical_query_count, 3);
    assert_eq!(outcome.lexical_elapsed_ms, 42);
}

#[test]
fn chunk_retrieval_lanes_continue_when_lexical_lane_fails() {
    let vector_chunk = runtime_chunk("vector", 0.9);

    let outcome = combine_chunk_retrieval_lanes(
        Ok((vec![vector_chunk.clone()], 17)),
        Err(anyhow::anyhow!("lexical backend unavailable")),
    )
    .expect("vector lane should keep retrieval usable");

    assert_eq!(outcome.degraded_lane_count, 1);
    assert_eq!(outcome.vector_hits.len(), 1);
    assert_eq!(outcome.vector_hits[0].chunk_id, vector_chunk.chunk_id);
    assert!(outcome.lexical_hits.is_empty());
    assert_eq!(outcome.vector_elapsed_ms, 17);
}

#[test]
fn chunk_retrieval_lanes_fail_when_both_lanes_fail() {
    let result = combine_chunk_retrieval_lanes(
        Err(anyhow::anyhow!("vector backend unavailable")),
        Err(anyhow::anyhow!("lexical backend unavailable")),
    );
    let error = match result {
        Ok(_) => panic!("both failed lanes must fail retrieval"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all chunk retrieval lanes failed"));
    assert!(message.contains("vector backend unavailable"));
    assert!(message.contains("lexical backend unavailable"));
}

#[test]
fn chunk_retrieval_fails_when_a_degraded_lane_leaves_no_evidence() {
    // Observed on stage under connection-pool exhaustion: the vector lane
    // failed, the lexical lane survived but matched nothing, and the turn
    // returned HTTP 200 with an answer built from document titles, zero
    // references, and a vacuous `verified` verdict. No evidence plus a known
    // lane failure must fail the turn instead.
    let result = combine_chunk_retrieval_lanes(
        Err(anyhow::anyhow!("pool timed out while waiting for an open connection")),
        Ok((Vec::new(), 8, 12)),
    );
    let error = match result {
        Ok(_) => panic!("a degraded lane with no surviving evidence must fail retrieval"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("chunk retrieval degraded to no evidence"));
    assert!(message.contains("pool timed out"));
}

#[test]
fn chunk_retrieval_allows_an_empty_bundle_when_both_lanes_ran_clean() {
    // A question that genuinely matches nothing is not a failure: both lanes
    // ran, neither degraded, and the empty result must stay a normal outcome.
    let outcome = combine_chunk_retrieval_lanes(Ok((Vec::new(), 9)), Ok((Vec::new(), 4, 11)))
        .expect("a clean search that matched nothing must remain a success");

    assert_eq!(outcome.degraded_lane_count, 0);
    assert!(outcome.vector_hits.is_empty());
    assert!(outcome.lexical_hits.is_empty());
}

#[test]
fn lexical_query_results_keep_successful_subqueries_when_one_fails() {
    let first = runtime_chunk("first", 0.9);
    let second = runtime_chunk("second", 0.7);

    let hits = combine_lexical_query_results(
        vec![
            Ok(vec![first.clone()]),
            Err(anyhow::anyhow!("one lexical subquery failed")),
            Ok(vec![second.clone()]),
        ],
        3,
        8,
    )
    .expect("successful lexical subqueries should keep the lane usable");

    let ids = hits.into_iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([first.chunk_id, second.chunk_id]));
}

#[test]
fn lexical_query_results_fail_when_all_subqueries_fail() {
    let result = combine_lexical_query_results(
        vec![
            Err(anyhow::anyhow!("first lexical subquery failed")),
            Err(anyhow::anyhow!("second lexical subquery failed")),
        ],
        2,
        8,
    );
    let error = match result {
        Ok(_) => panic!("all failed lexical subqueries must fail the lane"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all lexical chunk search queries failed"));
    assert!(message.contains("first lexical subquery failed"));
    assert!(message.contains("second lexical subquery failed"));
}

#[test]
fn lexical_query_results_fail_when_survivors_matched_nothing_after_a_failure() {
    // Partial subquery failure plus empty survivors is still "retrieval broke",
    // not "nothing matched". Reporting it as a clean empty lane would hide the
    // degradation from the lane-level evidence floor.
    let result = combine_lexical_query_results(
        vec![Ok(Vec::new()), Err(anyhow::anyhow!("pool timed out"))],
        2,
        16,
    );
    let error = match result {
        Ok(_) => panic!("a failed subquery with no surviving hits must fail the lane"),
        Err(error) => error,
    };

    assert!(format!("{error:#}").contains("lexical chunk search degraded to no evidence"));
}

#[test]
fn lexical_query_results_allow_an_empty_lane_when_every_subquery_ran_clean() {
    let hits = combine_lexical_query_results(vec![Ok(Vec::new()), Ok(Vec::new())], 2, 16)
        .expect("a clean lexical search that matched nothing must remain a success");

    assert!(hits.is_empty());
}

#[test]
fn query_ir_focus_search_results_keep_successful_subqueries_when_one_fails() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    let hits = combine_query_ir_focus_search_results(
        vec![
            Ok(vec![(first, 0.0)]),
            Err(anyhow::anyhow!("one focus subquery failed")),
            Ok(vec![(second, 0.7)]),
        ],
        3,
    )
    .expect("successful focus subqueries should keep the additive lane usable");

    let ids = hits.iter().map(|(chunk_id, _)| *chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([first, second]));
    assert_eq!(hits[0].1, query_ir_focus_chunk_score(0));
    assert_eq!(hits[1].1, 0.7);
}

#[test]
fn query_ir_focus_search_results_fail_when_all_subqueries_fail() {
    let result = combine_query_ir_focus_search_results(
        vec![
            Err(anyhow::anyhow!("first focus subquery failed")),
            Err(anyhow::anyhow!("second focus subquery failed")),
        ],
        2,
    );
    let error = match result {
        Ok(_) => panic!("all failed focus subqueries must fail the focus lane"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all query-IR focus chunk searches failed"));
    assert!(message.contains("first focus subquery failed"));
    assert!(message.contains("second focus subquery failed"));
}

pub(super) fn sample_document_row(file_name: &str, title: &str) -> KnowledgeDocumentRow {
    let document_id = Uuid::now_v7();
    KnowledgeDocumentRow {
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: document_id.to_string(),
        file_name: Some(file_name.to_string()),
        title: Some(title.to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        deleted_at: None,
    }
}

pub(super) fn knowledge_chunk_row(
    document: &KnowledgeDocumentRow,
    chunk_index: i32,
    chunk_kind: Option<&str>,
    text: &str,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        document_id: document.document_id,
        revision_id: document.readable_revision_id.unwrap_or_else(Uuid::now_v7),
        chunk_index,
        chunk_kind: chunk_kind.map(str::to_string),
        content_text: text.to_string(),
        normalized_text: text.to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(text.split_whitespace().count() as i32),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}

pub(super) fn runtime_graph_node(
    label: &str,
    node_type: &str,
    summary: Option<&str>,
) -> RuntimeGraphQueryNodeRow {
    RuntimeGraphQueryNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        identity_key: format!("{node_type}:{}", label.to_lowercase()),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: json!([]),
        summary: summary.map(str::to_string),
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub(super) fn runtime_graph_evidence_row(
    target_id: Uuid,
    evidence_text: &str,
) -> RuntimeGraphEvidenceRow {
    RuntimeGraphEvidenceRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        target_kind: "node".to_string(),
        target_id,
        document_id: Some(Uuid::now_v7()),
        chunk_id: Some(Uuid::now_v7()),
        source_file_name: Some("synthetic-source".to_string()),
        page_ref: None,
        evidence_text: evidence_text.to_string(),
        confidence_score: Some(1.0),
        created_at: Utc::now(),
    }
}

pub(super) fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        node_positions,
        Default::default(),
    )
}
