use super::test_prelude::*;

#[test]
fn query_ir_focus_ignores_weak_document_focus_when_typed_focus_exists() {
    let mut ir = release_query_ir(None, Some("Needle Server"));
    ir.document_focus = Some(DocumentHint { hint: "Neighbor Document".to_string() });

    let focus_queries = query_ir_lexical_focus_queries(&ir);
    let search_queries = query_ir_focus_search_queries(
        "broad question mentioning Neighbor Document",
        &focus_queries,
    );

    assert!(focus_queries.contains(&"Needle Server".to_string()));
    assert!(!focus_queries.contains(&"Neighbor Document".to_string()));
    assert_eq!(search_queries, vec!["Needle Server".to_string()]);
}

#[test]
fn configure_focus_queries_do_not_inject_low_idf_configuration_markers() {
    let mut ir = release_query_ir(None, None);
    ir.act = QueryAct::ConfigureHow;
    ir.scope = QueryScope::SingleDocument;
    ir.document_focus = Some(DocumentHint { hint: "Payment Gateway".to_string() });
    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "settlement connector".to_string(), role: EntityRole::Object }];

    let focus_queries = query_ir_lexical_focus_queries(&ir);

    assert!(focus_queries.contains(&"settlement connector".to_string()));
    assert!(focus_queries.contains(&"Payment Gateway".to_string()));
    assert!(!focus_queries.contains(&"http".to_string()));
    assert!(!focus_queries.contains(&"settlement connector http".to_string()));
    assert!(!focus_queries.contains(&"settlement connector .conf".to_string()));
}

#[test]
fn lexical_queries_preserve_ambiguous_alphanumeric_prefixes() {
    let plan = build_query_plan("Q16. Which ports should a terminal use?", None, None, None);
    let queries =
        build_lexical_queries("Q16. Which ports should a terminal use?", &plan, &[], None);

    assert_eq!(
        queries.first().map(String::as_str),
        Some("Q16. Which ports should a terminal use?")
    );
    assert!(queries.iter().any(|query| query.split_whitespace().any(|term| term == "Q16.")));
    assert!(plan.keywords.contains(&"q16".to_string()));
}

#[test]
fn entity_bio_filter_keeps_authoritative_graph_evidence_without_label_substring() {
    let evidence = runtime_chunk("configuration facts only", entity_bio_chunk_score(0));
    let lexical_false_positive = runtime_chunk("forest inventory", entity_bio_chunk_score(1));
    let retained = retain_entity_bio_candidates(
        vec![evidence.clone(), lexical_false_positive],
        &HashSet::from([evidence.chunk_id]),
        &["foster".to_string()],
    );

    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].chunk_id, evidence.chunk_id);
}

#[test]
fn entity_bio_filter_still_rejects_lexical_false_positive_without_label() {
    let lexical_false_positive = runtime_chunk("forest inventory", entity_bio_chunk_score(0));
    let retained = retain_entity_bio_candidates(
        vec![lexical_false_positive],
        &HashSet::new(),
        &["foster".to_string()],
    );

    assert!(retained.is_empty());
}

#[test]
fn document_identity_scores_stay_above_identity_floor_and_preserve_order() {
    let first = document_identity_chunk_score(0, 0);
    let second = document_identity_chunk_score(0, 1);
    let next_document = document_identity_chunk_score(1, 0);

    assert!(first >= DOCUMENT_IDENTITY_SCORE_FLOOR);
    assert!(first > second);
    assert!(second > next_document);
}
