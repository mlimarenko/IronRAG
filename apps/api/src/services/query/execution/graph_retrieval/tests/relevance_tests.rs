use super::*;

#[test]
fn lexical_entity_hits_match_node_types_from_query_ir() {
    let plan = build_query_plan("list graph inventory", None, Some(8), None);
    let ir = inventory_ir(&["event"]);
    let graph_index = graph_index_with_nodes(vec![
        node("[26.04.2026 22:36]", "event", Some("Timestamp marking a chat message")),
        node("setup guide", "artifact", None),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].label, "[26.04.2026 22:36]");
    assert_eq!(hits[0].node_type, "event");
}

#[test]
fn lexical_entity_hits_prioritize_wildcard_prefix_labels() {
    let plan = build_query_plan("list alpha-* modules", None, Some(8), None);
    let ir = wildcard_inventory_ir("alpha-*");
    let graph_index = graph_index_with_nodes(vec![
        node("Alpha Suite", "software_module", Some("Overview for alpha modules")),
        node("alpha-core", "software_module", None),
        node("alpha-desktop", "software_module", None),
        node("beta-core", "software_module", None),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = query_relevant_entity_hits(&plan, Some(&ir), &profiles, &graph_index, 2);
    let labels = hits.iter().map(|hit| hit.label.as_str()).collect::<Vec<_>>();

    assert_eq!(labels, vec!["alpha-core", "alpha-desktop"]);
}

#[test]
fn inventory_entity_hits_fall_back_to_supported_graph_nodes() {
    let plan = RuntimeQueryPlan {
        keywords: vec!["inventory".to_string()],
        entity_keywords: Vec::new(),
        ..build_query_plan("inventory", None, Some(8), None)
    };
    let ir = inventory_ir(&[]);
    let mut primary = node("Alpha Gateway", "software_module", None);
    primary.support_count = 9;
    let mut secondary = node("Beta Worker", "software_module", None);
    secondary.support_count = 4;
    let document = node("source.md", "document", None);
    let graph_index = graph_index_with_nodes(vec![secondary, document, primary]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = query_relevant_entity_hits(&plan, Some(&ir), &profiles, &graph_index, 2);
    let labels = hits.iter().map(|hit| hit.label.as_str()).collect::<Vec<_>>();

    assert_eq!(labels, vec!["Alpha Gateway", "Beta Worker"]);
}

#[test]
fn lexical_entity_hits_match_summary_and_node_type_not_only_label() {
    let plan = RuntimeQueryPlan {
        keywords: vec!["timestamp".to_string()],
        entity_keywords: Vec::new(),
        ..build_query_plan("timestamp inventory", None, Some(8), None)
    };
    let graph_index = graph_index_with_nodes(vec![node(
        "[2026-04-26 20:00]",
        "event",
        Some("Message timestamp extracted from a transcript"),
    )]);

    let hits = lexical_entity_hits(&plan, None, &[], &graph_index);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_type, "event");
}

#[test]
fn lexical_entity_hits_use_query_ir_focus_terms() {
    let plan = build_query_plan("how configure connector?", None, Some(8), None);
    let ir = configure_ir("shared reports", "report archive path");
    let graph_index = graph_index_with_nodes(vec![
        node("/srv/reports/archive", "artifact", Some("Path to shared report archive")),
        node("/srv/cache", "artifact", Some("Runtime cache path")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits[0].label, "/srv/reports/archive");
}

#[test]
fn lexical_entity_hits_promote_clipped_target_prefix_match() {
    let plan = build_query_plan("how configure acmew?", None, Some(8), None);
    let ir = configure_ir("Acmew", "Acmew");
    let graph_index = graph_index_with_nodes(vec![
        node("Acmealpha payment processor", "artifact", Some("Acmealpha payment settings")),
        node("Beta payment processor", "artifact", Some("Beta payment settings")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits[0].label, "Acmealpha payment processor");
    assert!(score_value(hits[0].score) > 100.0);
    assert!(
        hits.iter()
            .find(|hit| hit.label == "Beta payment processor")
            .is_none_or(|hit| score_value(hit.score) < score_value(hits[0].score))
    );
}

#[test]
fn graph_target_profiles_learn_related_tokens_from_document_labels() {
    let ir = configure_ir("Acmew", "Acmew");
    let graph_index = graph_index_with_nodes(vec![
        node("Acmealpha setup guide", "document", Some("Configuration source")),
        node("Beta setup guide", "document", Some("Configuration source")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Acmealpha setup guide",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );

    assert!(score > 0);
}

#[test]
fn graph_target_profiles_use_document_focus_instead_of_value_facets() {
    let ir = configure_ir_with_object_facets(
        "Acmew",
        &["installation", "configuration file", "parameters"],
    );
    let graph_index = graph_index_with_nodes(vec![
        node("Acmealpha setup guide", "document", Some("Configuration source")),
        node("Configuration file", "artifact", Some("Generic file reference")),
        node("Parameter catalog", "artifact", Some("Generic parameter reference")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let focused_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Acmealpha setup guide",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );
    let generic_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Configuration file",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );

    assert!(focused_score > 0);
    assert_eq!(generic_score, 0);
}

#[test]
fn graph_target_profiles_anchor_incomplete_focused_compare_to_document_focus() {
    let ir =
        focused_compare_ir("Acmew", &["module options", "operation rules", "limit matrix"], None);
    let graph_index = graph_index_with_nodes(vec![
        node("Acmealpha setup guide", "document", Some("Configuration source")),
        node("Module options", "artifact", Some("Generic module catalogue")),
        node("Operation rules", "artifact", Some("Generic operation catalogue")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let focused_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Acmealpha setup guide",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );
    let generic_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Module options",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );

    assert!(focused_score > 0);
    assert_eq!(generic_score, 0);
}

#[test]
fn graph_target_profiles_fallback_to_entities_when_focused_compare_document_is_absent() {
    let ir = focused_compare_ir("Missing setup source", &["Module options"], None);
    let graph_index = graph_index_with_nodes(vec![node(
        "Module options",
        "artifact",
        Some("Generic module catalogue"),
    )]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let entity_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Module options",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );

    assert!(entity_score > 0);
}

#[test]
fn graph_target_profiles_keep_complete_compare_operands() {
    let ir = focused_compare_ir("Acmew", &["Alpha Engine", "Beta Engine"], Some("Beta"));
    let graph_index = graph_index_with_nodes(vec![
        node("Acmealpha setup guide", "document", Some("Configuration source")),
        node("Alpha Engine", "artifact", Some("First compared option")),
        node("Beta Engine", "artifact", Some("Second compared option")),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let operand_score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "Alpha Engine",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );

    assert!(operand_score > 0);
}

#[test]
fn lexical_entity_hits_promote_explicit_target_and_rare_related_token() {
    let plan = build_query_plan("who is Alpha Omega?", None, Some(8), None);
    let ir = describe_ir("Alpha Omega");
    let graph_index = graph_index_with_nodes(vec![
        node("Alpha Omega", "person", None),
        node("Omega Delta", "person", None),
        node("Alpha Person", "person", None),
        node("Alpha Team", "person", None),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits[0].label, "Alpha Omega");
    let omega_index = hits.iter().position(|hit| hit.label == "Omega Delta").unwrap();
    let alpha_index = hits.iter().position(|hit| hit.label == "Alpha Person").unwrap();
    assert!(omega_index < alpha_index);
    assert!(score_value(hits[omega_index].score) > 100.0);
    assert!(score_value(hits[alpha_index].score) < 1.0);
}

#[test]
fn lexical_entity_hits_promote_multi_target_nodes_above_single_anchor_nodes() {
    let plan = build_query_plan("find Beacon near Harbor Delta", None, Some(8), None);
    let ir = describe_ir_with_targets(&["Beacon", "Harbor Delta"]);
    let graph_index = graph_index_with_nodes(vec![
        node("Beacon", "artifact", None),
        node("Harbor Delta", "location", None),
        node("Harbor Delta archive", "artifact", None),
        node("Beacon moved through Harbor Delta", "event", None),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits[0].label, "Beacon moved through Harbor Delta");
    assert!(
        score_value(hits[0].score) > score_value(hits[1].score),
        "distinct target-entity coverage must outrank one-anchor matches"
    );
}

#[test]
fn lexical_entity_hits_deduplicate_duplicate_target_entities() {
    let plan = build_query_plan("find Beacon", None, Some(8), None);
    let duplicate_ir = describe_ir_with_targets(&["Beacon", "Beacon"]);
    let single_ir = describe_ir("Beacon");
    let graph_index = graph_index_with_nodes(vec![node("Beacon", "artifact", None)]);

    let duplicate_profiles = graph_target_entity_profiles(Some(&duplicate_ir), &graph_index);
    let single_profiles = graph_target_entity_profiles(Some(&single_ir), &graph_index);
    let duplicate_hits =
        lexical_entity_hits(&plan, Some(&duplicate_ir), &duplicate_profiles, &graph_index);
    let single_hits = lexical_entity_hits(&plan, Some(&single_ir), &single_profiles, &graph_index);

    assert_eq!(score_value(duplicate_hits[0].score), score_value(single_hits[0].score));
}

#[test]
fn lexical_entity_hits_do_not_promote_embedded_short_target_labels() {
    let plan = build_query_plan("who is Sasha Otoya?", None, Some(8), None);
    let ir = describe_ir("Sasha Otoya");
    let graph_index = graph_index_with_nodes(vec![
        node("OTO", "organization", None),
        node("Alex Otoya", "person", None),
        node("Sasha Otoya", "person", None),
    ]);

    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);

    assert_eq!(hits[0].label, "Sasha Otoya");
    let embedded_index = hits.iter().position(|hit| hit.label == "OTO");
    let related_index = hits.iter().position(|hit| hit.label == "Alex Otoya").unwrap();
    assert!(
        embedded_index.is_none_or(|index| index > related_index),
        "embedded short label must not outrank token-overlap entity"
    );
}

#[test]
fn lexical_entity_hits_do_not_return_document_nodes() {
    let plan = build_query_plan("list graph inventory", None, Some(8), None);
    let ir = inventory_ir(&["document"]);
    let graph_index = graph_index_with_nodes(vec![node("chat.txt", "document", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    assert!(lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index).is_empty());
}
