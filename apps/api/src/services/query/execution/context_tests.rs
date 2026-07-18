use chrono::Utc;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryLanguage, QueryScope, QueryTargetKind,
    SourceSliceFilter, SourceSliceSpec,
};
use crate::{
    infra::repositories::RuntimeGraphQueryNodeRow,
    services::knowledge::runtime_read::ActiveRuntimeGraphProjection,
};

use super::*;

fn source_slice_ir(direction: SourceSliceDirection, count: u16) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction,
            count: Some(count),
            filter: SourceSliceFilter::None,
        }),
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn latest_version_slice_ir(count: u16) -> QueryIR {
    let mut ir = source_slice_ir(SourceSliceDirection::Tail, count);
    ir.scope = QueryScope::LibraryMeta;
    ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
    if let Some(slice) = ir.source_slice.as_mut() {
        slice.filter = SourceSliceFilter::ReleaseMarker;
    }
    ir
}

fn general_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn entity_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Person],
        target_entities: vec![EntityMention {
            label: "Project Omega".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn inventory_entity_ir(target_label: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Artifact],
        target_entities: vec![EntityMention {
            label: target_label.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn library_inventory_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn source_slice_unit(ordinal: i32, source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: ordinal,
        chunk_kind: Some("metadata_block".to_string()),
        document_label: "records.jsonl".to_string(),
        excerpt: source_text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: source_text.to_string(),
    }
}

fn latest_version_chunk(label: &str, chunk_index: i32, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: format!("{label} excerpt {chunk_index}"),
        score_kind: RuntimeChunkScoreKind::DocumentIdentity,
        score: Some(score),
        source_text: format!("{label} body {chunk_index}"),
    }
}

fn source_profile(source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("source_profile".to_string()),
        document_label: "records.jsonl".to_string(),
        excerpt: "[source_profile source_format=record_jsonl unit_count=2]".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(4.0),
        source_text: source_text.to_string(),
    }
}

fn ordinary_chunk(excerpt: &str, source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "guide.md".to_string(),
        excerpt: excerpt.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}

fn runtime_graph_node(
    label: &str,
    node_type: &str,
    summary: Option<&str>,
) -> RuntimeGraphQueryNodeRow {
    RuntimeGraphQueryNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        canonical_key: format!("{node_type}:{label}"),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: serde_json::json!([]),
        summary: summary.map(str::to_string),
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        node_positions,
        Default::default(),
    )
}

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
    assert!(lines[0].contains("alpha-core"));
    assert!(lines[0].contains("Runs the Alpha Suite core service."));
    assert!(lines[1].contains("alpha-sync"));
    assert!(!lines.join("\n").contains("beta-core"));
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
    assert!(lines.iter().any(|line| line.contains("alpha-089")));
    assert!(!lines.join("\n").contains("beta-000"));
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
    assert!(!lines.iter().any(|line| line.contains("alpha-064")));
}

#[test]
fn source_slice_context_renders_ordered_units_not_chunks() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, 2);
    let chunks = vec![
        source_slice_unit(2, "[unit_id=u-2] second"),
        source_slice_unit(3, "[unit_id=u-3] third"),
    ];

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "show latest records",
        &[],
        &[],
        &chunks,
        &[],
        4096,
    );

    assert!(context.contains("SOURCE_SLICE_UNIT"));
    assert!(context.contains("returned_unit_count: 2"));
    assert!(!context.contains("SOURCE_SLICE_CHUNK"));
    assert!(context.find("u-2").unwrap() < context.find("u-3").unwrap());
}

#[test]
fn source_slice_context_prefers_source_units_over_fallback_chunks() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, 1);
    let mut selected_unit = source_slice_unit(7, "[unit_id=u-7] selected record");
    selected_unit.chunk_kind = Some(super::super::SOURCE_UNIT_CHUNK_KIND.to_string());
    let chunks = vec![ordinary_chunk("fallback paragraph", "fallback paragraph"), selected_unit];

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "show latest record",
        &[],
        &[],
        &chunks,
        &[],
        4096,
    );

    assert!(context.contains("returned_unit_count: 1"));
    assert!(context.contains("[unit_id=u-7] selected record"));
    assert!(!context.contains("fallback paragraph"));
}

#[test]
fn source_slice_context_does_not_leak_profile_sample_units() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, 1);
    let chunks = vec![
        source_profile(
            "[source_profile source_format=record_jsonl unit_count=2]\n[unit_id=old] old sample",
        ),
        source_slice_unit(2, "[unit_id=u-2] latest unit"),
    ];

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "show latest record",
        &[],
        &[],
        &chunks,
        &[],
        4096,
    );

    assert!(context.contains("[source_profile source_format=record_jsonl unit_count=2]"));
    assert!(context.contains("[unit_id=u-2] latest unit"));
    assert!(!context.contains("[unit_id=old] old sample"));
}

#[test]
fn latest_version_source_slice_context_uses_runtime_rank_order() {
    let query_ir = latest_version_slice_ir(3);
    let chunks = vec![
        latest_version_chunk("Version 1.0.1", 0, 100.0),
        latest_version_chunk("Version 1.0.3", 0, 300.0),
        latest_version_chunk("Version 1.0.2", 0, 200.0),
        ordinary_chunk("unranked", "unranked"),
    ];

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "latest releases",
        &[],
        &[],
        &chunks,
        &[],
        4_000,
    );

    let newest = context.find("Version 1.0.3").unwrap();
    let middle = context.find("Version 1.0.2").unwrap();
    let oldest = context.find("Version 1.0.1").unwrap();
    assert!(newest < middle, "{context}");
    assert!(middle < oldest, "{context}");
    assert!(!context.contains("unranked"));
}

#[test]
fn bounded_context_ranks_source_units_by_question_focus_and_renders_source_text() {
    let query_ir = general_ir();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let unrelated = RuntimeMatchedChunk {
        document_id,
        revision_id,
        ..source_slice_unit(
            194,
            "[unit_id=later]\n44. video outline\n45. lesson plan\n46. music prompt",
        )
    };
    let correct_body = format!(
        "[unit_id=scripts]\n{}\n10. status report generator for ArcadeEditor beginners",
        "1. ArcadeEditor calculator script for beginners. ".repeat(12)
    );
    let correct = RuntimeMatchedChunk {
        document_id,
        revision_id,
        excerpt: excerpt_for(&correct_body, 120),
        ..source_slice_unit(6, &correct_body)
    };
    let chunks = vec![unrelated, correct];

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "simple ArcadeEditor scripts for beginners",
        &[],
        &[],
        &chunks,
        &[],
        8192,
    );

    assert!(context.find("unit_id=scripts").unwrap() < context.find("unit_id=later").unwrap());
    assert!(context.contains("10. status report generator"));
}

#[test]
fn bounded_context_keeps_ordinary_chunks_on_excerpt_text() {
    let context = assemble_bounded_context(
        &[],
        &[],
        &[ordinary_chunk("short excerpt", "short excerpt plus hidden source body")],
        4096,
    );

    assert!(context.contains("short excerpt"));
    assert!(!context.contains("hidden source body"));
}

#[test]
fn bounded_context_keeps_source_context_block_text() {
    let mut chunk = ordinary_chunk(
        "Alpha Devices: Device A",
        &format!(
            "{}\nAlpha Devices:\n- Device A\n- Device B\n- Device C\n- Device D",
            "preface ".repeat(160)
        ),
    );
    chunk.score_kind = RuntimeChunkScoreKind::SourceContext;

    let context = assemble_bounded_context_for_query(
        &general_ir(),
        "Which Alpha Devices are listed?",
        &[],
        &[],
        &[chunk],
        &[],
        8192,
    );

    assert!(context.contains("[document source_context"));
    assert!(context.contains("Device A"));
    assert!(context.contains("Device D"));
}

#[test]
fn bounded_context_renders_document_identity_chunks_with_source_unit_budget() {
    let source = format!(
        "{}\nInstall the module:\nsample-install alpha-connector\n\nConfiguration file: /opt/alpha/modules/connector/connector.conf\n[Main]\nendpointUrl = https://alpha.example.test/api\npartnerId = demo-partner",
        "Subject Alpha overview. ".repeat(80)
    );
    let mut chunk = ordinary_chunk("Subject Alpha setup", &source);
    chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;

    let context = assemble_bounded_context_for_query(
        &general_ir(),
        "How do I configure Subject Alpha?",
        &[],
        &[],
        &[chunk],
        &[],
        8192,
    );

    assert!(context.contains("[document document_identity"));
    assert!(context.contains("sample-install alpha-connector"));
    assert!(context.contains("/opt/alpha/modules/connector/connector.conf"));
    assert!(context.contains("[Main]"));
    assert!(context.contains("partnerId = demo-partner"));
}

#[test]
fn bounded_context_orders_document_identity_chunks_by_retrieval_score() {
    let mut overview = ordinary_chunk("Subject Alpha overview", "Subject Alpha overview.");
    overview.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    overview.score = Some(1200.0);
    overview.chunk_index = 0;

    let mut setup = ordinary_chunk(
        "Subject Alpha configuration",
        "Install the module:\nsample-install alpha-connector\nConfiguration file: /opt/alpha/modules/connector/connector.conf",
    );
    setup.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    setup.score = Some(2600.0);
    setup.document_id = overview.document_id;
    setup.revision_id = overview.revision_id;
    setup.chunk_index = 1;

    let chunks = vec![overview, setup];
    let ordered = order_bounded_context_chunks(
        "How do I configure Subject Alpha?",
        &general_ir(),
        &chunks,
        &[],
    );

    assert_eq!(ordered.first().map(|chunk| chunk.chunk_index), Some(1));
}

#[test]
fn bounded_context_prioritizes_content_anchor_before_identity_headers() {
    let mut query_ir = general_ir();
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.retrieval_query = Some(
            "List service plans from the section «Pricing policy: subscription plans». Include plan names."
                .to_string(),
        );

    let mut identity_noise = ordinary_chunk(
        "Image loading rules",
        &format!(
            "{}\nImage loading begins near the viewport. Add DNS records for the hosted domain.",
            "Navigation and unrelated page chrome. ".repeat(80)
        ),
    );
    identity_noise.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    identity_noise.score = Some(1_000_000.0);
    identity_noise.document_label = "FAQ index".to_string();
    identity_noise.chunk_index = 0;

    let mut related_body = ordinary_chunk(
        "Pricing policy: subscription plans",
        "Pricing policy: subscription plans\n\
             The service can be used for free.\n\
             Personal plan includes forms and integrations.\n\
             Business plan includes multiple projects and code export.",
    );
    related_body.score = Some(1.0);
    related_body.document_label = "Product overview".to_string();
    related_body.chunk_index = 33;

    let context = assemble_bounded_context_for_query(
        &query_ir,
        "what subscription plans are available?",
        &[],
        &[],
        &[identity_noise, related_body],
        &[],
        900,
    );

    assert!(context.contains("Personal plan"), "{context}");
    assert!(context.contains("Business plan"), "{context}");
    assert!(
        context.find("Pricing policy: subscription plans").unwrap()
            < context.find("Image loading").unwrap_or(usize::MAX),
        "{context}"
    );
}

#[test]
fn bounded_context_uses_unquoted_question_tokens_for_content_anchors() {
    let mut identity_noise = ordinary_chunk(
        "Hosted domain troubleshooting",
        &format!(
            "{}\nHosted domain troubleshooting covers DNS records and image loading behavior.",
            "General navigation text. ".repeat(80)
        ),
    );
    identity_noise.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    identity_noise.score = Some(1_000_000.0);

    let mut related_body = ordinary_chunk(
        "Subscription plan overview",
        "Subscription plans\n\
             Free plan covers publishing with platform branding.\n\
             Personal plan adds forms and integrations.\n\
             Business plan adds multiple projects and export options.",
    );
    related_body.score = Some(1.0);
    related_body.chunk_index = 12;

    let context = assemble_bounded_context_for_query(
        &general_ir(),
        "what subscription plans are available?",
        &[],
        &[],
        &[identity_noise, related_body],
        &[],
        900,
    );

    assert!(context.contains("Free plan"), "{context}");
    assert!(context.contains("Business plan"), "{context}");
}

#[test]
fn procedure_context_prioritizes_command_runbook_before_long_noise() {
    let mut query_ir = general_ir();
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    query_ir.target_entities = vec![EntityMention {
        label: "Alpha subject server".to_string(),
        role: EntityRole::Subject,
    }];
    query_ir.retrieval_query = Some("how to update Alpha subject server?".to_string());

    let mut noise = ordinary_chunk(
        "Alpha subject server reference",
        &format!(
            "Alpha subject server reference. {}",
            "Long field description with request examples. ".repeat(120)
        ),
    );
    noise.score = Some(100.0);
    noise.chunk_index = 1;

    let mut runbook = ordinary_chunk(
        "Alpha subject server versioned update",
        "Alpha subject server update:\n\
             1. Install package alpha-upgrade command: sample-install alpha-upgrade\n\
             2. Run update script from /opt/alpha/bin: cd /opt/alpha/bin ./upgrade_alpha.sh",
    );
    runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    runbook.score = Some(1.0);
    runbook.chunk_index = 21;

    let chunks = vec![noise, runbook];
    let context = assemble_bounded_context_for_query(
        &query_ir,
        "how to update Alpha subject server?",
        &[],
        &[],
        &chunks,
        &[],
        900,
    );

    assert!(context.contains("sample-install alpha-upgrade"), "{context}");
    assert!(context.contains("./upgrade_alpha.sh"), "{context}");
}

#[test]
fn procedure_context_model_ignores_scoped_previous_question_terms() {
    let mut query_ir = general_ir();
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    query_ir.target_entities =
        vec![EntityMention { label: "Beta Service".to_string(), role: EntityRole::Subject }];

    let bare = ProcedureContextModel::new("how to update Beta Service version?", &query_ir);
    let scoped = ProcedureContextModel::new(
        "scope: how to update Alpha Suite\nquestion: how to update Beta Service version?",
        &query_ir,
    );

    assert_eq!(scoped.action_terms, bare.action_terms);
    assert_eq!(scoped.subject_terms, bare.subject_terms);
    assert!(!scoped.action_terms.contains("alpha"));
    assert!(!scoped.action_terms.contains("suite"));
}

#[test]
fn retrieved_document_brief_preview_keeps_near_intro_identifiers() {
    let source = format!(
        "{}GatewayModuleAlpha is the installable module for Subject Alpha.",
        "Introductory setup context. ".repeat(12)
    );
    let chunk = ordinary_chunk("Subject Alpha setup overview.", &source);
    let preview = focused_preview_from_bundle_chunks(&[&chunk]).unwrap();

    assert!(preview.contains("GatewayModuleAlpha"));
}

#[test]
fn entity_target_context_prioritizes_graph_lines_before_documents() {
    let context = assemble_bounded_context_for_query(
        &entity_ir(),
        "Project Omega",
        &[
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
        &[],
        &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        &[],
        4096,
    );

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
    let context = assemble_bounded_context_for_query(
        &entity_ir(),
        "Project Omega",
        &[],
        &[],
        &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        &graph_evidence_lines,
        4096,
    );

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
    let context = assemble_bounded_context_for_query(
        &inventory_entity_ir("alpha-*"),
        "List alpha-* modules",
        &[
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
        &[],
        &[ordinary_chunk(
            "Alpha Suite overview mentions several modules.",
            "Alpha Suite overview mentions several modules.",
        )],
        &graph_evidence_lines,
        512,
    );

    let match_index = context.find("[entity-match prefix] alpha-core").unwrap();
    let node_index = context.find("[graph-node] alpha-core").unwrap();
    assert!(match_index < node_index);
    if let Some(evidence_index) = context.find("[graph-evidence") {
        assert!(node_index < evidence_index);
    }
}

#[test]
fn library_inventory_context_prioritizes_graph_nodes_without_target_entities() {
    let context = assemble_bounded_context_for_query(
        &library_inventory_ir(),
        "List graph inventory",
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "Alpha Gateway".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(0.9),
        }],
        &[],
        &[ordinary_chunk(
            "A long document overview also exists.",
            "A long document overview also exists.",
        )],
        &[],
        4096,
    );

    let graph_index = context.find("[graph-node] Alpha Gateway").unwrap();
    let document_index = context.find("[document]").unwrap();
    assert!(graph_index < document_index);
}

#[test]
fn graph_node_context_includes_entity_summary_as_answer_evidence() {
    let context = assemble_bounded_context_for_query(
        &library_inventory_ir(),
        "List graph inventory",
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "Alpha Worker".to_string(),
            node_type: "artifact".to_string(),
            summary: Some("Runs queued jobs and retries failed deliveries.".to_string()),
            score: Some(0.9),
        }],
        &[],
        &[ordinary_chunk(
            "A long document overview also exists.",
            "A long document overview also exists.",
        )],
        &[],
        4096,
    );

    assert!(context.contains("[graph-node] evidence:"));
    assert!(context.contains("Runs queued jobs and retries failed deliveries."));
    assert!(context.contains("entity_hint: Alpha Worker (artifact)"));
}

#[test]
fn entity_target_context_marks_exact_and_token_overlap_matches() {
    let context = assemble_bounded_context_for_query(
        &entity_ir(),
        "Project Omega",
        &[
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
        &[],
        &[ordinary_chunk(
            "Project Omega appears in a long planning note.",
            "Project Omega appears in a long planning note.",
        )],
        &[],
        4096,
    );

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
    let context = assemble_bounded_context_for_query(
        &ir,
        "Sasha Otoya",
        &[
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
        &[],
        &[ordinary_chunk("Sasha Otoya is mentioned once.", "Sasha Otoya is mentioned once.")],
        &[],
        4096,
    );

    assert!(!context.contains("[entity-match exact] OTO"));
    assert!(context.contains("[entity-match token-overlap] Alex Otoya"));
}

#[test]
fn bounded_context_renders_query_focused_source_text_for_ordinary_chunks() {
    let hidden_rules = "retail_clock rules: register once at start and once at finish.";
    let source_text = format!(
        "{}\n{}",
        "introductory material without the requested rule. ".repeat(20),
        hidden_rules
    );
    let context = assemble_bounded_context_for_query(
        &general_ir(),
        "what are the retail_clock rules?",
        &[],
        &[],
        &[ordinary_chunk("introductory material without details", &source_text)],
        &[],
        4096,
    );

    assert!(context.contains(hidden_rules));
}
