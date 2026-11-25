use super::*;
use crate::services::query::execution::RuntimeMatchedRelationship;

#[test]
fn target_entity_context_lines_surface_explicit_graph_summaries() {
    let mut query_ir = entity_ir();
    query_ir.target_entities = vec![
        EntityMention { label: "alpha-core".to_string(), role: EntityRole::Object },
        EntityMention { label: "alpha-sync".to_string(), role: EntityRole::Object },
    ];
    let graph_index = graph_index_with_nodes(vec![
        runtime_graph_node("alpha-core", "artifact", Some("Runs the Alpha Suite core service.")),
        runtime_graph_node("alpha-sync", "artifact", Some("Synchronizes Alpha Suite records.")),
        runtime_graph_node("beta-core", "artifact", Some("Unrelated component.")),
    ]);

    let lines = target_entity_context_lines(&query_ir, &graph_index);

    assert_eq!(lines.len(), 2);
    assert!(lines[0].rendered().contains("alpha-core"));
    assert!(lines[0].rendered().contains("Runs the Alpha Suite core service."));
    assert!(lines[1].rendered().contains("alpha-sync"));
    assert!(lines.iter().all(|line| !line.rendered().contains("beta-core")));
}

#[test]
fn target_entity_context_lines_pair_summary_evidence_with_the_rendered_line() {
    let mut query_ir = entity_ir();
    query_ir.target_entities = vec![
        EntityMention { label: "alpha-core".to_string(), role: EntityRole::Object },
        EntityMention { label: "alpha-sync".to_string(), role: EntityRole::Object },
    ];
    let graph_index = graph_index_with_nodes(vec![
        runtime_graph_node("alpha-core", "artifact", Some("Runs the Alpha Suite core service.")),
        runtime_graph_node("alpha-sync", "artifact", None),
    ]);

    let lines = target_entity_context_lines(&query_ir, &graph_index);

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].evidence_text(), Some("Runs the Alpha Suite core service."));
    assert_eq!(lines[0].passage_evidence_text(), None);
    assert_eq!(
        lines[0].rendered(),
        "[graph-node] target_match=explicit evidence: Runs the Alpha Suite core service. \
         | entity_hint: alpha-core (artifact)"
    );
    assert_eq!(lines[1].evidence_text(), None);
    assert_eq!(lines[1].passage_evidence_text(), None);
    assert_eq!(
        lines[1].rendered(),
        "[graph-node] target_match=explicit entity_hint: alpha-sync (artifact)"
    );
}

#[test]
fn wildcard_inventory_target_context_expands_matching_graph_nodes() {
    let mut query_ir = inventory_entity_ir("alpha-*");
    query_ir.scope = QueryScope::SingleDocument;
    let mut nodes = (0..90)
        .map(|index| {
            runtime_graph_node(
                &format!("alpha-{index:03}"),
                "artifact",
                Some("Installable Alpha Suite module."),
            )
        })
        .collect::<Vec<_>>();
    nodes.push(runtime_graph_node("beta-000", "artifact", Some("Unrelated Beta Suite module.")));
    let graph_index = graph_index_with_nodes(nodes);

    let lines = target_entity_context_lines(&query_ir, &graph_index);

    assert!(lines.len() > TARGET_ENTITY_CONTEXT_LINE_LIMIT);
    assert!(lines.iter().any(|line| line.rendered().contains("alpha-089")));
    assert!(lines.iter().all(|line| !line.rendered().contains("beta-000")));
}

#[test]
fn descriptive_wildcard_target_context_keeps_default_cap() {
    let mut query_ir = inventory_entity_ir("alpha-*");
    query_ir.act = QueryAct::Describe;
    let nodes = (0..90)
        .map(|index| {
            runtime_graph_node(
                &format!("alpha-{index:03}"),
                "artifact",
                Some("Installable Alpha Suite module."),
            )
        })
        .collect::<Vec<_>>();
    let graph_index = graph_index_with_nodes(nodes);

    let lines = target_entity_context_lines(&query_ir, &graph_index);

    assert_eq!(lines.len(), TARGET_ENTITY_CONTEXT_LINE_LIMIT);
    assert!(!lines.iter().any(|line| line.rendered().contains("alpha-064")));
}

#[test]
fn entity_target_context_prioritizes_graph_lines_before_documents() {
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &entity_ir(),
        question: "Project Omega",
        entities: &[
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Project Omega".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Project Omega Peer".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.8),
            },
        ],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    let graph_index = context.find("[graph-node]").unwrap_or_default();
    let second_graph_index = context.find("Project Omega Peer").unwrap_or_default();
    let document_index = context.find("[document]").unwrap_or_default();
    assert!(graph_index < document_index);
    assert!(second_graph_index < document_index);
}

#[test]
fn entity_target_context_keeps_unanchored_graph_evidence_before_documents() {
    let graph_evidence_lines = vec![
        "[graph-evidence target=\"Project Omega\"]\nProject Omega has a rare one-row fact."
            .to_string(),
    ];
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &entity_ir(),
        question: "Project Omega",
        entities: &[],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        graph_evidence_lines: &graph_evidence_lines,
        budget_chars: 4096,
    });

    let evidence_index = context.find("[graph-evidence").unwrap();
    let document_index = context.find("[document]").unwrap();
    assert!(evidence_index < document_index);
    assert!(context.contains("rare one-row fact"));
}

#[test]
fn inventory_context_keeps_matching_graph_nodes_before_long_evidence() {
    let graph_evidence_lines = vec![format!(
        "[graph-evidence target=\"Alpha Suite\"]\n{}",
        "Long supporting evidence. ".repeat(40)
    )];
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &inventory_entity_ir("alpha-*"),
        question: "List alpha-* modules",
        entities: &[
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "alpha-core".to_string(),
                node_type: "artifact".to_string(),
                summary: None,
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "alpha-desktop".to_string(),
                node_type: "artifact".to_string(),
                summary: None,
                score: Some(0.8),
            },
        ],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "Alpha Suite overview mentions several modules.",
            "Alpha Suite overview mentions several modules.",
        )],
        graph_evidence_lines: &graph_evidence_lines,
        budget_chars: 512,
    });

    let match_index = context.find("[entity-match prefix] alpha-core").unwrap();
    let node_index = context.find("[graph-node] alpha-core").unwrap();
    assert!(match_index < node_index);
    if let Some(evidence_index) = context.find("[graph-evidence") {
        assert!(node_index < evidence_index);
    }
}

#[test]
fn library_inventory_context_prioritizes_graph_nodes_without_target_entities() {
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &library_inventory_ir(),
        question: "List graph inventory",
        entities: &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "Alpha Gateway".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(0.9),
        }],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "A long document overview also exists.",
            "A long document overview also exists.",
        )],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    let graph_index = context.find("[graph-node] Alpha Gateway").unwrap();
    let document_index = context.find("[document]").unwrap();
    assert!(graph_index < document_index);
}

#[test]
fn graph_node_context_includes_entity_summary_as_answer_evidence() {
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &library_inventory_ir(),
        question: "List graph inventory",
        entities: &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "Alpha Worker".to_string(),
            node_type: "artifact".to_string(),
            summary: Some("Runs queued jobs and retries failed deliveries.".to_string()),
            score: Some(0.9),
        }],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "A long document overview also exists.",
            "A long document overview also exists.",
        )],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    assert!(context.contains("[graph-node] evidence:"));
    assert!(context.contains("Runs queued jobs and retries failed deliveries."));
    assert!(context.contains("entity_hint: Alpha Worker (artifact)"));
}

#[test]
fn entity_target_context_marks_exact_and_token_overlap_matches() {
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &entity_ir(),
        question: "Project Omega",
        entities: &[
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Project Omega".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Omega Delta".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.8),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Project Alpha".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.7),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Project Beta".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.6),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Unrelated Sigma".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.1),
            },
        ],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    let exact_index = context.find("[entity-match exact] Project Omega").unwrap();
    let related_index = context.find("[entity-match token-overlap] Omega Delta").unwrap();
    let graph_index = context.find("[graph-node]").unwrap();
    assert!(exact_index < graph_index);
    assert!(related_index < graph_index);
    assert!(!context.contains("[entity-match token-overlap] Project Alpha"));
    assert!(!context.contains("[entity-match token-overlap] Project Beta"));
    assert!(!context.contains("[entity-match token-overlap] Unrelated Sigma"));
}

#[test]
fn entity_target_context_rejects_embedded_short_exact_match() {
    let mut ir = entity_ir();
    ir.target_entities[0].label = "Sasha Otoya".to_string();
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &ir,
        question: "Sasha Otoya",
        entities: &[
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "OTO".to_string(),
                node_type: "organization".to_string(),
                summary: None,
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::now_v7(),
                label: "Alex Otoya".to_string(),
                node_type: "person".to_string(),
                summary: None,
                score: Some(0.8),
            },
        ],
        relationships: &[],
        chunks: &[ordinary_chunk(
            "Sasha Otoya is mentioned once.",
            "Sasha Otoya is mentioned once.",
        )],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    assert!(!context.contains("[entity-match exact] OTO"));
    assert!(context.contains("[entity-match token-overlap] Alex Otoya"));
}

#[test]
fn graph_edge_context_marks_only_short_recorded_wording_as_exact() {
    const SHORT_STATEMENT: &str =
        "Alpha Sync does not publish records until Beta Gateway confirms readiness.";
    const LONG_STATEMENT: &str = "Alpha Sync records queue depth for every shard and keeps the previous reading for comparison.\nBeta Gateway reports readiness only after the shard list is complete and the probe window closes.\nGamma Worker replays the last successful batch when the gateway reports an incomplete shard list.";
    let context = assemble_bounded_context(
        &[],
        &[
            RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "depends_on".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "Alpha Sync".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "Beta Gateway".to_string(),
                summary: Some(SHORT_STATEMENT.to_string()),
                support_count: 3,
                score: Some(0.7),
            },
            RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "reports".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "Alpha Sync".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "Beta Gateway".to_string(),
                summary: Some(LONG_STATEMENT.to_string()),
                support_count: 1,
                score: Some(0.65),
            },
            RuntimeMatchedRelationship {
                edge_id: Uuid::now_v7(),
                relation_type: "uses".to_string(),
                from_node_id: Uuid::now_v7(),
                from_label: "Gamma Worker".to_string(),
                to_node_id: Uuid::now_v7(),
                to_label: "Delta Queue".to_string(),
                summary: None,
                support_count: 2,
                score: Some(0.6),
            },
        ],
        &[ordinary_chunk("Alpha Sync waits for the gateway.", "Alpha Sync waits for the gateway.")],
        4_096,
    );

    let exact_marker = RuntimeMatchedRelationship::EXACT_WORDING_FIELD;
    assert!(context.contains(&format!(
        "[graph-edge evidence] evidence: {SHORT_STATEMENT} | {exact_marker} | relation_hint: Alpha Sync --depends_on--> Beta Gateway | support_count: 3"
    )));
    assert!(context.contains(&format!(
        "[graph-edge evidence] evidence: {LONG_STATEMENT} | relation_hint: Alpha Sync --reports--> Beta Gateway"
    )));
    assert_eq!(
        context.matches(exact_marker).count(),
        1,
        "a statement past the protected-span limit stays ordinary evidence"
    );
    assert!(context.contains(
        "[graph-edge relation_hint] Gamma Worker --uses--> Delta Queue | support_count: 2"
    ));
}
