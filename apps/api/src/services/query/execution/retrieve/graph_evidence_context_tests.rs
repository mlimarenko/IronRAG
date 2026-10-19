use super::test_prelude::*;

#[test]
fn graph_evidence_targets_preserve_graph_order_and_dedupe() {
    let node_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
    let entities = vec![
        RuntimeMatchedEntity {
            node_id,
            label: "Archive".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(1.0),
        },
        RuntimeMatchedEntity {
            node_id,
            label: "Archive".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(0.5),
        },
    ];
    let relationships = vec![RuntimeMatchedRelationship {
        edge_id,
        relation_type: "mentions".to_string(),
        from_node_id: node_id,
        from_label: "Guide".to_string(),
        to_node_id: node_id,
        to_label: "Archive".to_string(),
        summary: None,
        support_count: 1,
        score: Some(0.8),
    }];

    let targets = graph_evidence_targets(&entities, &relationships);

    assert_eq!(targets, vec![("node".to_string(), node_id), ("edge".to_string(), edge_id)]);
}

#[test]
fn graph_evidence_targets_for_query_include_lexical_node_outside_visible_bundle() {
    let noise = runtime_graph_node("Common Topic", "concept", None);
    let needle = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let graph_index = graph_index_with_nodes(vec![noise, needle.clone()]);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };

    let targets = graph_evidence_targets_for_query(&[], &[], &plan, None, &[], &graph_index);

    assert!(targets.contains(&("node".to_string(), needle.id)));
}

#[test]
fn graph_evidence_targets_for_query_keep_retrieved_bundle_targets_first() {
    let bundle_node = runtime_graph_node("Selected Bundle Target", "artifact", None);
    let query_node = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let graph_index = graph_index_with_nodes(vec![bundle_node.clone(), query_node.clone()]);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };
    let entities = vec![RuntimeMatchedEntity {
        node_id: bundle_node.id,
        label: bundle_node.label.clone(),
        node_type: bundle_node.node_type.clone(),
        summary: None,
        score: Some(0.1),
    }];

    let targets = graph_evidence_targets_for_query(&entities, &[], &plan, None, &[], &graph_index);

    assert_eq!(targets.first(), Some(&("node".to_string(), bundle_node.id)));
    assert!(targets.contains(&("node".to_string(), query_node.id)));
}

#[test]
fn graph_evidence_targets_for_query_return_full_bundle_without_supplemental_targets() {
    let (_, query_node, graph_index, plan, entities) = graph_target_pressure_fixture();

    let targets = graph_evidence_targets_for_query(&entities, &[], &plan, None, &[], &graph_index);

    assert_eq!(targets.len(), 48);
    assert!(!targets.contains(&("node".to_string(), query_node.id)));
}

#[test]
fn graph_evidence_targets_for_query_prioritize_explicit_targets_under_cap_pressure() {
    let (bundle_nodes, query_node, graph_index, plan, entities) = graph_target_pressure_fixture();
    let ir = target_entities_query_ir(&["Needle"]);
    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let targets = graph_evidence_targets_for_query(
        &entities,
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert_eq!(targets.len(), 48);
    assert_eq!(targets.first(), Some(&("node".to_string(), query_node.id)));
    assert!(targets.contains(&("node".to_string(), bundle_nodes[0].id)));
}

fn graph_target_pressure_fixture() -> (
    Vec<RuntimeGraphQueryNodeRow>,
    RuntimeGraphQueryNodeRow,
    QueryGraphIndex,
    RuntimeQueryPlan,
    Vec<RuntimeMatchedEntity>,
) {
    let bundle_nodes = (0..48)
        .map(|index| runtime_graph_node(&format!("Bundle target {index:02}"), "artifact", None))
        .collect::<Vec<_>>();
    let query_node = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let mut graph_nodes = bundle_nodes.clone();
    graph_nodes.push(query_node.clone());
    let graph_index = graph_index_with_nodes(graph_nodes);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };
    let entities = bundle_nodes
        .iter()
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: None,
            score: Some(0.1),
        })
        .collect::<Vec<_>>();
    (bundle_nodes, query_node, graph_index, plan, entities)
}

#[test]
fn graph_evidence_targets_for_query_can_use_document_nodes_as_source_anchors() {
    let bundle_nodes = (0..48)
        .map(|index| runtime_graph_node(&format!("Bundle target {index:02}"), "artifact", None))
        .collect::<Vec<_>>();
    let target_document =
        runtime_graph_node("Acmealpha setup guide", "document", Some("Focused configuration"));
    let mut graph_nodes = bundle_nodes.clone();
    graph_nodes.push(target_document.clone());
    let graph_index = graph_index_with_nodes(graph_nodes);
    let plan = build_query_plan("show the acmew configuration steps", None, Some(8), None);
    let entities = bundle_nodes
        .iter()
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: None,
            score: Some(0.1),
        })
        .collect::<Vec<_>>();
    let ir = target_entities_query_ir(&["Acmew"]);
    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let targets = graph_evidence_targets_for_query(
        &entities,
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert_eq!(targets.first(), Some(&("node".to_string(), target_document.id)));
}

#[test]
fn graph_evidence_targets_for_query_keep_multi_anchor_node_under_target_cap_pressure() {
    let composite = runtime_graph_node("Beacon crossed Harbor Delta", "event", None);
    let mut nodes = (0..80)
        .map(|index| {
            runtime_graph_node(&format!("Harbor Delta reference {index:02}"), "artifact", None)
        })
        .collect::<Vec<_>>();
    nodes.push(composite.clone());
    let graph_index = graph_index_with_nodes(nodes);
    let plan = build_query_plan("find Beacon near Harbor Delta", None, Some(8), None);
    let ir = target_entities_query_ir(&["Beacon", "Harbor Delta"]);

    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let targets = graph_evidence_targets_for_query(
        &[],
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert!(
        targets.contains(&("node".to_string(), composite.id)),
        "multi-anchor graph node must survive graph evidence target cap pressure"
    );
}

#[test]
fn graph_evidence_text_replaces_weak_support_chunk_text() {
    let mut chunk = runtime_chunk("document title without the requested literal", 1.0);
    let chunk_id = chunk.chunk_id;
    let mut evidence_texts_by_chunk = HashMap::new();
    evidence_texts_by_chunk.insert(
        chunk_id,
        vec!["Needle setting: alpha.path = /srv/alpha and port = 9407".to_string()],
    );

    apply_graph_evidence_texts_to_chunks(
        std::slice::from_mut(&mut chunk),
        &evidence_texts_by_chunk,
        &["alpha".to_string(), "9407".to_string()],
        &["alpha".to_string(), "9407".to_string()],
    );

    assert!(chunk.source_text.starts_with("Needle setting: alpha.path"));
    assert!(chunk.source_text.contains("Source chunk:"));
    assert!(chunk.excerpt.contains("alpha.path"));
}

#[test]
fn graph_evidence_text_overlay_focuses_long_mixed_rows() {
    let mut chunk = runtime_chunk("generic support chunk", 1.0);
    let chunk_id = chunk.chunk_id;
    let filler = (0..80)
        .map(|index| format!("Generic section {index}: profile forms and archive notes."))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut evidence_texts_by_chunk = HashMap::new();
    evidence_texts_by_chunk.insert(
        chunk_id,
        vec![format!(
            "{filler} | Sample connectors: Option One, Option Two, custom route | More unrelated material."
        )],
    );

    apply_graph_evidence_texts_to_chunks(
        std::slice::from_mut(&mut chunk),
        &evidence_texts_by_chunk,
        &["sample".to_string(), "connectors".to_string()],
        &["sample".to_string(), "connectors".to_string(), "route".to_string()],
    );

    assert!(chunk.source_text.contains("Sample connectors"));
    assert!(chunk.source_text.contains("custom route"));
    assert!(!chunk.source_text.contains("Generic section 0"));
}

#[test]
fn graph_evidence_chunk_hits_use_ranked_text_rows_as_chunk_candidates() {
    let target_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let mut row = runtime_graph_evidence_row(
        target_id,
        "Needle setting: alpha.path = /srv/alpha and port = 9407",
    );
    row.chunk_id = Some(chunk_id);

    let (hits, evidence_texts_by_chunk) = graph_evidence_chunk_hits_from_rows(&[row]);

    assert_eq!(hits, vec![(chunk_id, graph_evidence_chunk_score(0))]);
    assert_eq!(
        evidence_texts_by_chunk.get(&chunk_id).and_then(|texts| texts.first()),
        Some(&"Needle setting: alpha.path = /srv/alpha and port = 9407".to_string())
    );
}

#[test]
fn graph_evidence_source_document_ids_preserve_rank_order_without_duplicates() {
    let target_id = Uuid::now_v7();
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let mut first = runtime_graph_evidence_row(target_id, "Alpha setup overview");
    first.document_id = Some(first_document_id);
    first.chunk_id = None;
    let mut duplicate = runtime_graph_evidence_row(target_id, "Alpha setup detail");
    duplicate.document_id = Some(first_document_id);
    duplicate.chunk_id = Some(Uuid::now_v7());
    let mut second = runtime_graph_evidence_row(target_id, "Beta setup overview");
    second.document_id = Some(second_document_id);
    second.chunk_id = None;
    let mut orphan = runtime_graph_evidence_row(target_id, "Detached summary");
    orphan.document_id = None;

    let document_ids = graph_evidence_source_document_ids(&[first, duplicate, orphan, second]);

    assert_eq!(document_ids, vec![first_document_id, second_document_id]);
}

#[test]
fn graph_evidence_source_document_ids_with_priority_prepend_target_documents() {
    let target_id = Uuid::now_v7();
    let ranked_first_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let mut ranked_first = runtime_graph_evidence_row(target_id, "Broad setup overview");
    ranked_first.document_id = Some(ranked_first_document_id);
    let mut ranked_target = runtime_graph_evidence_row(target_id, "Focused setup detail");
    ranked_target.document_id = Some(target_document_id);

    let document_ids = graph_evidence_source_document_ids_with_priority(
        &[target_document_id],
        &[ranked_first, ranked_target],
    );

    assert_eq!(document_ids, vec![target_document_id, ranked_first_document_id]);
}

#[test]
fn graph_evidence_text_search_document_scope_prefers_explicit_targets() {
    let explicit_a = Uuid::now_v7();
    let explicit_b = Uuid::now_v7();
    let derived_a = Uuid::now_v7();
    let derived_b = Uuid::now_v7();
    let explicit = BTreeSet::from([explicit_a, explicit_b]);

    let scoped =
        graph_evidence_text_search_document_scope(&explicit, &[derived_a, derived_b, derived_a]);

    assert_eq!(scoped, vec![explicit_a, explicit_b]);
}

#[test]
fn graph_evidence_text_search_document_scope_dedupes_derived_targets() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    let scoped =
        graph_evidence_text_search_document_scope(&BTreeSet::new(), &[first, second, first]);

    assert_eq!(scoped, vec![first, second]);
}

#[test]
fn graph_evidence_context_line_formats_delimited_row_fields() {
    let graph_index = graph_index_with_nodes(Vec::new());
    let document_id = Uuid::now_v7();
    let row = RuntimeGraphEvidenceRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        target_kind: "node".to_string(),
        target_id: Uuid::now_v7(),
        document_id: Some(document_id),
        chunk_id: Some(Uuid::now_v7()),
        source_file_name: Some("alpha-source".to_string()),
        page_ref: None,
        evidence_text: "Column Alpha: keep A | Column Beta: keep B".to_string(),
        confidence_score: Some(1.0),
        created_at: Utc::now(),
    };
    let source_labels =
        HashMap::from([(document_id, "https://example.test/docs/alpha".to_string())]);

    let line = graph_evidence_context_line(&row, &graph_index, &source_labels, &[])
        .expect("graph evidence line");

    assert!(line.contains("[graph-evidence source=\"https://example.test/docs/alpha\"]"));
    assert!(!line.contains("alpha-source"));
    assert!(line.contains("- Column Alpha: keep A"));
    assert!(line.contains("- Column Beta: keep B"));
}

#[test]
fn graph_evidence_context_line_focuses_long_mixed_rows() {
    let target = runtime_graph_node("Sample Subject", "artifact", None);
    let graph_index = graph_index_with_nodes(vec![target.clone()]);
    let filler = (0..80)
        .map(|index| format!("Generic section {index}: record forms and archive notes."))
        .collect::<Vec<_>>()
        .join(" | ");
    let row = runtime_graph_evidence_row(
        target.id,
        &format!(
            "{filler} | Sample connectors: Option One, Option Two, custom route | More unrelated material after the focused sentence."
        ),
    );

    let line = graph_evidence_context_line(
        &row,
        &graph_index,
        &HashMap::new(),
        &["sample".to_string(), "connectors".to_string(), "route".to_string()],
    )
    .expect("graph evidence line");

    assert!(line.contains("Sample connectors"));
    assert!(line.contains("custom route"));
    assert!(!line.contains("Generic section 0"));
}

#[test]
fn graph_evidence_source_document_ids_from_scored_targets_promote_structural_target_document() {
    let broad_target = runtime_graph_node("General configuration", "concept", None);
    let focused_target = runtime_graph_node("Needle gateway", "artifact", None);
    let graph_index = graph_index_with_nodes(vec![broad_target.clone(), focused_target.clone()]);
    let target_entity_profiles =
        graph_target_entity_profiles(Some(&target_entities_query_ir(&["Needle"])), &graph_index);
    let broad_document_id = Uuid::now_v7();
    let focused_document_id = Uuid::now_v7();
    let mut broad = runtime_graph_evidence_row(
        broad_target.id,
        "Configuration appendix | file: /srv/common.ini | option: enabled",
    );
    broad.document_id = Some(broad_document_id);
    broad.source_file_name = Some("general configuration".to_string());
    let mut focused = runtime_graph_evidence_row(
        focused_target.id,
        "Gateway setup | file: /srv/needle.ini | option: enabled",
    );
    focused.document_id = Some(focused_document_id);
    focused.source_file_name = Some("needle gateway setup".to_string());

    let document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &[broad, focused],
        "Where is the Needle gateway configuration file?",
        &["Needle gateway configuration".to_string()],
        &graph_index,
        &target_entity_profiles,
    );

    assert_eq!(document_ids.first(), Some(&focused_document_id));
}

#[test]
fn graph_evidence_source_document_ids_from_scored_targets_skip_zero_signal_documents() {
    let target = runtime_graph_node("Archive index", "concept", None);
    let graph_index = graph_index_with_nodes(vec![target.clone()]);
    let mut row = runtime_graph_evidence_row(target.id, "Archive index | owner: operations");
    row.document_id = Some(Uuid::now_v7());

    let document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &[row],
        "Where is the Needle gateway configuration file?",
        &["Needle gateway configuration".to_string()],
        &graph_index,
        &[],
    );

    assert!(document_ids.is_empty());
}

#[test]
fn graph_evidence_context_ranking_prefers_specific_rare_row_over_repeated_generic_rows() {
    let specific = runtime_graph_node("Alpha mismatch status", "condition", None);
    let generic = runtime_graph_node("Checkout behavior", "field", None);
    let graph_index = graph_index_with_nodes(vec![specific.clone(), generic.clone()]);
    let generic_body =
        "Row 12 | Status: Unknown status | Sale behavior: hold item | Return behavior: hold item";
    let generic_a = runtime_graph_evidence_row(generic.id, generic_body);
    let generic_b = runtime_graph_evidence_row(generic.id, generic_body);
    let exact = runtime_graph_evidence_row(
        specific.id,
        "Row 7 | Status: Alpha mismatch status | Sale behavior: hold item | Return behavior: release item",
    );

    let ranked = rank_graph_evidence_context_rows(GraphEvidenceContextRequest {
        text_evidence: &[generic_a, generic_b, exact.clone()],
        target_evidence: &[],
        question: "What are sale and return behavior for Alpha mismatch status?",
        text_queries: &["Alpha mismatch status".to_string()],
        graph_index: &graph_index,
        target_entity_profiles: &[],
        limit: 4,
    });

    assert_eq!(ranked.first().map(|row| row.id), Some(exact.id));
    assert_eq!(ranked.len(), 2);
}

#[test]
fn graph_evidence_context_ranking_keeps_source_metadata_below_evidence_body() {
    let specific = runtime_graph_node("Beta release channel", "condition", None);
    let generic = runtime_graph_node("Release notes", "document", None);
    let graph_index = graph_index_with_nodes(vec![specific.clone(), generic.clone()]);
    let mut source_only = runtime_graph_evidence_row(
        generic.id,
        "Row 2 | Status: general note | Action: inspect archive",
    );
    source_only.source_file_name = Some("Beta release channel index".to_string());
    let exact = runtime_graph_evidence_row(
        specific.id,
        "Row 4 | Status: Beta release channel | Action: enable canary rollout",
    );

    let ranked = rank_graph_evidence_context_rows(GraphEvidenceContextRequest {
        text_evidence: &[source_only, exact.clone()],
        target_evidence: &[],
        question: "Which action belongs to Beta release channel?",
        text_queries: &["Beta release channel".to_string()],
        graph_index: &graph_index,
        target_entity_profiles: &[],
        limit: 4,
    });

    assert_eq!(ranked.first().map(|row| row.id), Some(exact.id));
}

#[test]
fn graph_evidence_context_ranking_keeps_multi_anchor_target_row_under_body_dedupe_pressure() {
    let composite = runtime_graph_node("Beacon crossed Harbor Delta", "event", None);
    let generic = runtime_graph_node("Harbor Delta", "location", None);
    let graph_index = graph_index_with_nodes(vec![generic.clone(), composite.clone()]);
    let shared_evidence = "event: Beacon crossed Harbor Delta after the calibration window closed";
    let generic_row = runtime_graph_evidence_row(generic.id, shared_evidence);
    let composite_row = runtime_graph_evidence_row(composite.id, shared_evidence);
    let ir = target_entities_query_ir(&["Beacon", "Harbor Delta"]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let ranked = rank_graph_evidence_context_rows(GraphEvidenceContextRequest {
        text_evidence: &[generic_row],
        target_evidence: std::slice::from_ref(&composite_row),
        question: "Which event involved Beacon and Harbor Delta?",
        text_queries: &["Beacon Harbor Delta".to_string()],
        graph_index: &graph_index,
        target_entity_profiles: &profiles,
        limit: 1,
    });

    assert_eq!(ranked.first().map(|row| row.id), Some(composite_row.id));
}
