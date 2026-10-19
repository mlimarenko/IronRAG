use super::*;

#[test]
fn bounded_context_skips_oversized_block_and_keeps_scanning_for_later_facts() {
    let oversized = format!("oversized-profile {}", "padding ".repeat(40));
    let later_fact = "directly responsive fact".to_string();

    let context =
        interleave_bounded_context_lines(&[], &[oversized, later_fact.clone()], 80, false, &[]);

    assert!(context.contains(&later_fact), "{context}");
    assert!(
        !context.contains("oversized-profile"),
        "the first-block excerpt is only a fallback when no complete block fits: {context}"
    );
}

#[test]
fn bounded_context_graph_first_skip_preserves_later_document_evidence() {
    let oversized_graph = format!("oversized-graph {}", "padding ".repeat(40));
    let graph_fact = "short graph fact".to_string();
    let document_fact = "short document fact".to_string();

    let context = interleave_bounded_context_lines(
        &[oversized_graph, graph_fact.clone()],
        std::slice::from_ref(&document_fact),
        80,
        true,
        &[],
    );

    assert!(context.contains(&graph_fact), "{context}");
    assert!(context.contains(&document_fact), "{context}");
    assert!(context.find(&graph_fact) < context.find(&document_fact), "{context}");
}

#[test]
fn bounded_context_uses_first_block_excerpt_when_nothing_fits() {
    let oversized = format!("only-oversized-block {}", "padding ".repeat(40));

    let context = interleave_bounded_context_lines(&[], &[oversized], 80, false, &[]);

    assert!(!context.is_empty());
    assert!(context.contains("only-oversized-block"), "{context}");
    assert!(context.len() <= 80, "{context}");
}

#[test]
fn source_slice_context_renders_ordered_units_not_chunks() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, 2);
    let chunks = vec![
        source_slice_unit(2, "[unit_id=u-2] second"),
        source_slice_unit(3, "[unit_id=u-3] third"),
    ];

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "show latest records",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    assert!(context.contains("SOURCE_SLICE_UNIT"));
    assert!(context.contains("returned_unit_count: 2"));
    assert!(!context.contains("SOURCE_SLICE_CHUNK"));
    assert!(context.find("u-2").unwrap() < context.find("u-3").unwrap());
}

#[test]
fn source_slice_context_prefers_source_units_over_fallback_chunks() {
    let query_ir = source_slice_ir(SourceSliceDirection::Tail, 1);
    let mut selected_unit = source_slice_unit(7, "[unit_id=u-7] selected record");
    selected_unit.chunk_kind = Some(super::super::super::SOURCE_UNIT_CHUNK_KIND.to_string());
    let chunks = vec![ordinary_chunk("fallback paragraph", "fallback paragraph"), selected_unit];

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "show latest record",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "show latest record",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "latest releases",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 4_000,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "simple ArcadeEditor scripts for beginners",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 8192,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &general_ir(),
        question: "Which Alpha Devices are listed?",
        entities: &[],
        relationships: &[],
        chunks: &[chunk],
        graph_evidence_lines: &[],
        budget_chars: 8192,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &general_ir(),
        question: "How do I configure Subject Alpha?",
        entities: &[],
        relationships: &[],
        chunks: &[chunk],
        graph_evidence_lines: &[],
        budget_chars: 8192,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "what subscription plans are available?",
        entities: &[],
        relationships: &[],
        chunks: &[identity_noise, related_body],
        graph_evidence_lines: &[],
        budget_chars: 900,
    });

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

    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &general_ir(),
        question: "what subscription plans are available?",
        entities: &[],
        relationships: &[],
        chunks: &[identity_noise, related_body],
        graph_evidence_lines: &[],
        budget_chars: 900,
    });

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
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "how to update Alpha subject server?",
        entities: &[],
        relationships: &[],
        chunks: &chunks,
        graph_evidence_lines: &[],
        budget_chars: 900,
    });

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
fn bounded_context_renders_query_focused_source_text_for_ordinary_chunks() {
    let hidden_rules = "retail_clock rules: register once at start and once at finish.";
    let source_text = format!(
        "{}\n{}",
        "introductory material without the requested rule. ".repeat(20),
        hidden_rules
    );
    let context = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &general_ir(),
        question: "what are the retail_clock rules?",
        entities: &[],
        relationships: &[],
        chunks: &[ordinary_chunk("introductory material without details", &source_text)],
        graph_evidence_lines: &[],
        budget_chars: 4096,
    });

    assert!(context.contains(hidden_rules));
}
