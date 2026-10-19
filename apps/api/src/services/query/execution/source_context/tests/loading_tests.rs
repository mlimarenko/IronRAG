use super::*;

#[test]
fn natural_language_diagnostic_literal_does_not_request_code_pattern_context() {
    let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ErrorCode];
    query_ir.literal_constraints = vec![LiteralSpan {
        text: "record was already processed".to_string(),
        kind: LiteralKind::Other,
    }];

    assert!(!requests_code_pattern_source_context(&query_ir));
}

#[test]
fn mislabeled_hyphenated_prose_does_not_request_code_pattern_context() {
    let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ErrorCode];
    query_ir.literal_constraints = vec![LiteralSpan {
        text: "record-was-already-processed".to_string(),
        kind: LiteralKind::Identifier,
    }];

    assert!(!requests_code_pattern_source_context(&query_ir));
}

#[test]
fn structured_error_identifier_requests_code_pattern_context() {
    let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ErrorCode];
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "ERR_204".to_string(), kind: LiteralKind::Identifier }];

    assert!(requests_code_pattern_source_context(&query_ir));
    assert_eq!(code_pattern_literal_terms(&query_ir), vec!["err_204"]);
}

#[test]
fn code_pattern_candidate_documents_are_bounded_and_keep_retrieval_order() {
    let revision_id = Uuid::from_u128(100);
    let documents = (1..=5).map(Uuid::from_u128).collect::<Vec<_>>();
    let chunks = vec![
        runtime_chunk(documents[0], revision_id, 0),
        runtime_chunk(documents[0], revision_id, 1),
        runtime_chunk(documents[1], revision_id, 2),
        runtime_chunk(documents[2], revision_id, 3),
        runtime_chunk(documents[3], revision_id, 4),
        runtime_chunk(documents[4], revision_id, 5),
    ];

    assert_eq!(
        code_pattern_candidate_document_ids(&chunks),
        documents[..SOURCE_CONTEXT_DOCUMENT_LIMIT].to_vec()
    );
}

#[test]
fn transport_pattern_candidate_documents_are_bounded_and_keep_retrieval_order() {
    let revision_id = Uuid::from_u128(200);
    let documents = (11..=15).map(Uuid::from_u128).collect::<Vec<_>>();
    let chunks = vec![
        runtime_chunk(documents[0], revision_id, 0),
        runtime_chunk(documents[0], revision_id, 1),
        runtime_chunk(documents[1], revision_id, 2),
        runtime_chunk(documents[2], revision_id, 3),
        runtime_chunk(documents[3], revision_id, 4),
        runtime_chunk(documents[4], revision_id, 5),
    ];

    assert_eq!(
        transport_pattern_candidate_document_ids(&chunks),
        documents[..SOURCE_CONTEXT_DOCUMENT_LIMIT].to_vec()
    );
}

#[test]
fn optional_code_pattern_failure_is_suppressed_with_primary_evidence() {
    let result = preserve_primary_context_on_code_pattern_failure::<u8>(
        Err(anyhow::anyhow!("synthetic storage failure")),
        true,
    );

    assert_eq!(result.expect("optional enrichment must fail soft"), Vec::<u8>::new());
}

#[test]
fn code_pattern_failure_without_primary_evidence_remains_fatal() {
    let result = preserve_primary_context_on_code_pattern_failure::<u8>(
        Err(anyhow::anyhow!("synthetic storage failure")),
        false,
    );

    assert!(result.is_err());
}

#[test]
fn optional_transport_pattern_failure_is_suppressed_with_primary_evidence() {
    let result = preserve_primary_context_on_transport_pattern_failure::<u8>(
        Err(anyhow::anyhow!("synthetic storage failure")),
        true,
    );

    assert_eq!(result.expect("optional enrichment must fail soft"), Vec::<u8>::new());
}

#[test]
fn transport_pattern_failure_without_primary_evidence_remains_fatal() {
    let result = preserve_primary_context_on_transport_pattern_failure::<u8>(
        Err(anyhow::anyhow!("synthetic storage failure")),
        false,
    );

    assert!(result.is_err());
}

#[test]
fn configuration_file_targets_request_path_source_context() {
    let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ConfigurationFile];

    assert!(
        requests_path_source_context(&query_ir),
        "setup answers that ask for a configuration file need path-bearing chunks even without a path literal"
    );
}

#[test]
fn low_confidence_fallback_expands_only_when_retrieved_chunks_have_structured_literals() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    query_ir.confidence = 0.25;
    let mut structured = runtime_chunk(document_id, revision_id, 7);
    structured.chunk_kind = Some("paragraph".to_string());
    structured.excerpt = "[UI.Alpha.Form]\nalphaFlag = true".to_string();
    structured.source_text = structured.excerpt.clone();
    let mut plain = runtime_chunk(document_id, revision_id, 8);
    plain.chunk_kind = Some("paragraph".to_string());
    plain.excerpt = "alpha beta gamma".to_string();
    plain.source_text = plain.excerpt.clone();

    assert!(requests_fallback_structured_source_context(Some(&query_ir), &[structured]));
    assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[plain]));

    query_ir.confidence = 0.9;
    assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[]));
}

#[test]
fn low_confidence_fallback_ignores_structured_chunk_kind_without_literal_surface() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    query_ir.confidence = 0.25;
    let mut code = runtime_chunk(document_id, revision_id, 7);
    code.chunk_kind = Some("code_block".to_string());
    code.excerpt = "sample alpha beta gamma".to_string();
    code.source_text = code.excerpt.clone();
    let mut table = runtime_chunk(document_id, revision_id, 8);
    table.chunk_kind = Some("table_row".to_string());
    table.excerpt = "Alpha Beta Gamma".to_string();
    table.source_text = table.excerpt.clone();

    assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[code]));
    assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[table]));
}

#[test]
fn table_row_summary_describe_ir_requests_expanded_source_context() {
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::TableRow,
        crate::domains::query_ir::QueryTargetKind::TableSummary,
    ];

    assert!(requests_table_row_source_context(&query_ir));
    assert_eq!(
        structured_source_context_top_k(&query_ir, 5),
        procedural_source_context_chunk_floor()
    );

    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::TableAverage];
    query_ir.act = QueryAct::RetrieveValue;

    assert!(!requests_table_row_source_context(&query_ir));
}

#[test]
fn table_row_summary_context_expands_candidate_document_limit() {
    assert_eq!(
        source_context_candidate_document_limit_for_request(false, true),
        SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
    );
    assert_eq!(
        source_context_candidate_document_limit_for_request(false, false),
        SOURCE_CONTEXT_DOCUMENT_LIMIT
    );
}

#[test]
fn table_row_summary_retrieve_value_ir_requests_expanded_source_context() {
    let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::TableRow,
        crate::domains::query_ir::QueryTargetKind::TableSummary,
    ];

    assert!(requests_table_row_source_context(&query_ir));
    assert_eq!(
        structured_source_context_top_k(&query_ir, 5),
        procedural_source_context_chunk_floor()
    );
}

#[test]
fn typed_low_confidence_structured_context_expands_lower_ranked_documents() {
    let docs = [
        (Uuid::now_v7(), Uuid::now_v7()),
        (Uuid::now_v7(), Uuid::now_v7()),
        (Uuid::now_v7(), Uuid::now_v7()),
        (Uuid::now_v7(), Uuid::now_v7()),
    ];
    let chunks = docs
        .iter()
        .enumerate()
        .map(|(rank, (document_id, revision_id))| {
            let mut chunk = runtime_chunk(*document_id, *revision_id, rank as i32 + 10);
            chunk.chunk_kind = Some("paragraph".to_string());
            chunk.source_text = if rank == 3 {
                "[UI.Alpha.Form]\nalphaFlag = true".to_string()
            } else {
                "unstructured filler".to_string()
            };
            chunk.excerpt = chunk.source_text.clone();
            chunk
        })
        .collect::<Vec<_>>();
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.confidence = 0.25;

    assert!(requests_fallback_structured_source_context(Some(&query_ir), &chunks));
    let default_candidates = collect_source_context_candidates(&chunks);
    assert!(!default_candidates.iter().any(|candidate| candidate.document_id == docs[3].0));

    let expanded_candidates = collect_source_context_candidates_with_limit(
        &chunks,
        source_context_candidate_document_limit(true),
        0,
    );
    assert!(expanded_candidates.iter().any(|candidate| candidate.document_id == docs[3].0));
}

#[test]
fn fallback_structured_search_keeps_short_token_literal_rows() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let existing =
        chunk_row(document_id, revision_id, 1, "paragraph", "[UI.Alpha]\nexistingFlag = true\nQ1");
    let plain = chunk_row(document_id, revision_id, 2, "paragraph", "Q1 filler text only");
    let literal =
        chunk_row(document_id, revision_id, 3, "paragraph", "[UI.Alpha]\nalphaFlag = true\nQ1");
    let existing_chunk_ids = HashSet::from([existing.chunk_id]);

    let selected = select_fallback_structured_search_rows(
        vec![(existing, 20.0), (plain, 100.0), (literal, 10.0)],
        &["q1".to_string()],
        &existing_chunk_ids,
        4,
    );

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0.chunk_index, 3);
    assert!(selected[0].1 > 0);
}

#[test]
fn configure_how_expands_preceding_setup_context() {
    let query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
    let span = source_context_neighbor_span(Some(&query_ir));
    let anchors = vec![SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 0 }];

    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(source_context_neighbor_windows(&anchors, span).contains(&(19, 23)));
    assert!(
        source_context_best_neighbor_score(&anchors, 19, span).is_some(),
        "procedural answers need the setup block that precedes the matching detail chunk"
    );
}

#[test]
fn fallback_structured_context_expands_preceding_setup_context() {
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.confidence = 0.25;
    let span = source_context_neighbor_span_for_request(Some(&query_ir), true);
    let anchors = vec![SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 0 }];

    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(source_context_neighbor_windows(&anchors, span).contains(&(19, 23)));
    assert!(
        source_context_best_neighbor_score(&anchors, 19, span).is_some(),
        "fallback structured answers need preceding key/value blocks near the matching detail"
    );
}

#[test]
fn configuration_target_types_expand_source_context_without_configure_act() {
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        crate::domains::query_ir::QueryTargetKind::ConfigKey,
    ];
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: "Provider Alpha".to_string() });
    let span = source_context_neighbor_span(Some(&query_ir));

    assert!(requests_expanded_source_context(&query_ir));
    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(
        structured_source_context_top_k(&query_ir, 3) > 3,
        "typed configuration answers need room for nearby key/value and code chunks"
    );
}

#[test]
fn anchorless_configuration_describe_keeps_default_source_context() {
    let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ConfigKey];

    assert!(!requests_expanded_source_context(&query_ir));
    assert_eq!(structured_source_context_top_k(&query_ir, 3), 3);
}

#[test]
fn error_code_intent_expands_source_context_like_setup_questions() {
    let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::ErrorCode];
    let span = source_context_neighbor_span(Some(&query_ir));

    assert!(requests_expanded_source_context(&query_ir));
    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(
        structured_source_context_top_k(&query_ir, 3) > 3,
        "typed diagnostic lookups need enough room for graph-source companions"
    );
}

#[test]
fn transport_inventory_intent_expands_source_context_like_setup_questions() {
    let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Port,
        crate::domains::query_ir::QueryTargetKind::Protocol,
        crate::domains::query_ir::QueryTargetKind::Connection,
    ];
    let span = source_context_neighbor_span(Some(&query_ir));

    assert!(requests_expanded_source_context(&query_ir));
    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(
        structured_source_context_top_k(&query_ir, 3) > 3,
        "transport inventory lookups need room for URL and port-bearing companions"
    );
}

#[test]
fn code_pattern_query_terms_keep_short_digit_anchors() {
    let terms = code_pattern_query_terms(
        &[
            "E101".to_string(),
            "8583".to_string(),
            "362".to_string(),
            "error".to_string(),
            "codes".to_string(),
            "card".to_string(),
        ],
        6,
    );

    assert_eq!(terms, vec!["e101", "8583", "362", "error", "codes", "card"]);
}

#[test]
fn non_procedural_source_context_stays_narrow() {
    let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
    let span = source_context_neighbor_span(Some(&query_ir));
    let anchors = vec![SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 0 }];

    assert_eq!(
        span,
        SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
        }
    );
    assert!(source_context_neighbor_windows(&anchors, span).contains(&(21, 23)));
    assert!(
        source_context_best_neighbor_score(&anchors, 19, span).is_none(),
        "default descriptive context should not silently widen evidence windows"
    );
}
