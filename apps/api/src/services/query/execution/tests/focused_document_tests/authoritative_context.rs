use super::*;

#[test]
fn build_authoritative_answer_context_does_not_filter_on_weak_document_focus() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();

    let context = build_authoritative_answer_context(
        "Which search engines and assistants or services are named as examples in the knowledge graph article?",
        &generic_query_ir(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![
                KnowledgeStructuredBlockRow {
                    normalized_text:
                        "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named.".to_string(),
                    text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                        .to_string(),
                    heading_trail: vec!["Examples".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeStructuredBlockRow {
                    normalized_text: "LLMs generate, summarize, translate, and reason over text."
                        .to_string(),
                    text: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                    heading_trail: vec!["Capabilities".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    display_value: "Google".to_string(),
                    normalized_value_text: "Google".to_string(),
                    normalized_value_exact: "Google".to_string(),
                    normalized_value_json: serde_json::json!("Google"),
                    fact_kind: "example".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    display_value: "translate".to_string(),
                    normalized_value_text: "translate".to_string(),
                    normalized_value_exact: "translate".to_string(),
                    normalized_value_json: serde_json::json!("translate"),
                    fact_kind: "capability".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: focused_document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: other_document_id,
                document_label: "large_language_model_wikipedia.md".to_string(),
                excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.2),
                source_text: "LLMs generate, summarize, translate, and reason over text."
                    .to_string(),
            },
        ],
        &[],
    );

    assert!(context.contains("Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa"));
    assert!(context.contains("LLMs generate, summarize, translate, and reason over text."));
    assert!(context.contains("capability: `translate`"));
    assert!(!context.contains("Focused grounded document"));
}

#[test]
fn build_authoritative_answer_context_keeps_unfocused_procedure_variants_across_documents() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["concept", "procedure"],
    );
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Connector".to_string(), role: EntityRole::Subject }];
    query_ir.document_focus = None;

    let context = build_authoritative_answer_context(
        "How do I configure Sample Connector?",
        &query_ir,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("paragraph".to_string()),
                document_id: primary_document_id,
                document_label: "Sample Connector Atlas setup".to_string(),
                excerpt: "1. Install atlas-adapter.pkg.\n2. Set atlasMode=true.".to_string(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: Some(10.0),
                source_text: "1. Install atlas-adapter.pkg.\n2. Set atlasMode=true.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("paragraph".to_string()),
                document_id: primary_document_id,
                document_label: "Sample Connector Atlas setup".to_string(),
                excerpt: "3. Run atlas-check --strict.".to_string(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: Some(9.0),
                source_text: "3. Run atlas-check --strict.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("paragraph".to_string()),
                document_id: secondary_document_id,
                document_label: "Sample Connector Boreal setup".to_string(),
                excerpt: "1. Install boreal-adapter.pkg.\n2. Set borealMode=true.".to_string(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "1. Install boreal-adapter.pkg.\n2. Set borealMode=true.".to_string(),
            },
        ],
        &[],
    );

    assert!(!context.contains("Focused grounded document"), "{context}");
    assert!(context.contains("atlas-adapter.pkg"), "{context}");
    assert!(context.contains("boreal-adapter.pkg"), "{context}");
}

#[test]
fn build_authoritative_answer_context_keeps_value_queries_broad_without_explicit_document() {
    let dominant_document_id = Uuid::now_v7();
    let value_document_id = Uuid::now_v7();
    let query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["table_row", "table_summary"],
    );

    let context = build_authoritative_answer_context(
        "Which value is reported for the target row?",
        &query_ir,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("table_row".to_string()),
                document_id: dominant_document_id,
                document_label: "dominant-table.csv".to_string(),
                excerpt: "Sheet: Main | Row 1 | Name: Alpha | Value: 10".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(10.0),
                source_text: "Sheet: Main | Row 1 | Name: Alpha | Value: 10".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("table_row".to_string()),
                document_id: dominant_document_id,
                document_label: "dominant-table.csv".to_string(),
                excerpt: "Sheet: Main | Row 2 | Name: Beta | Value: 20".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(9.0),
                source_text: "Sheet: Main | Row 2 | Name: Beta | Value: 20".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("table_row".to_string()),
                document_id: value_document_id,
                document_label: "value-table.csv".to_string(),
                excerpt: "Sheet: Detail | Row 1 | Name: Target | Value: 42".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Sheet: Detail | Row 1 | Name: Target | Value: 42".to_string(),
            },
        ],
        &[],
    );

    assert!(!context.contains("Focused grounded document"));
    assert!(context.contains("Name: Alpha"));
    assert!(context.contains("Name: Target"));
}

#[test]
fn build_authoritative_answer_context_filters_only_explicit_document_reference() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let context = build_authoritative_answer_context(
        "Summarize knowledge_graph_wikipedia.md",
        &generic_query_ir(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: focused_document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Focused document evidence.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.4),
                source_text: "Focused document evidence.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: other_document_id,
                document_label: "other_notes.md".to_string(),
                excerpt: "Other document evidence.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(2.0),
                source_text: "Other document evidence.".to_string(),
            },
        ],
        &[],
    );

    assert!(context.contains("Focused grounded document\n- knowledge_graph_wikipedia.md"));
    assert!(context.contains("Focused document evidence."));
    assert!(!context.contains("Other document evidence."));
}

#[test]
fn build_authoritative_answer_context_uses_query_ir_focus_for_follow_up_document_scope() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["package", "configuration_file", "config_key"],
    );
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha".to_string() });
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha".to_string(), role: EntityRole::Object }];
    query_ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        surface: "that setup".to_string(),
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
    }];

    let context = build_authoritative_answer_context(
        "show the setup instructions",
        &query_ir,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![
                KnowledgeStructuredBlockRow {
                    normalized_text:
                        "Alpha connector uses /opt/alpha/connector.conf with apiUrl and retryTimeout."
                            .to_string(),
                    text:
                        "Alpha connector uses /opt/alpha/connector.conf with apiUrl and retryTimeout."
                            .to_string(),
                    heading_trail: vec!["Alpha setup".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeStructuredBlockRow {
                    normalized_text: "Beta connector uses beta-adapter.pkg and beta.conf."
                        .to_string(),
                    text: "Beta connector uses beta-adapter.pkg and beta.conf.".to_string(),
                    heading_trail: vec!["Beta setup".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: focused_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: focused_document_id,
                document_label: "Provider Alpha setup.md".to_string(),
                excerpt:
                    "Install alpha-adapter.pkg, then edit /opt/alpha/connector.conf.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
                score: Some(1.0),
                source_text:
                    "Install alpha-adapter.pkg, then edit /opt/alpha/connector.conf.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: other_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: other_document_id,
                document_label: "Provider Beta setup.md".to_string(),
                excerpt: "Install beta-adapter.pkg, then edit /opt/beta/connector.conf.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
                score: Some(2.0),
                source_text:
                    "Install beta-adapter.pkg, then edit /opt/beta/connector.conf.".to_string(),
            },
        ],
        &[
            "[graph-evidence target=\"Provider Beta setup\"] Install beta-adapter.pkg".to_string(),
            "[graph-evidence target=\"Provider Alpha setup\"] apiUrl is required".to_string(),
        ],
    );

    assert!(context.contains("Focused grounded document\n- Provider Alpha setup.md"));
    assert!(context.contains("alpha-adapter.pkg"));
    assert!(context.contains("/opt/alpha/connector.conf"));
    assert!(context.contains("apiUrl is required"));
    assert!(!context.contains("beta-adapter.pkg"), "{context}");
    assert!(!context.contains("/opt/beta/connector.conf"), "{context}");
}

#[test]
fn build_authoritative_answer_context_does_not_lock_ir_focus_on_tied_documents() {
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file"],
    );
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha".to_string() });

    let context = build_authoritative_answer_context(
        "show the setup instructions",
        &query_ir,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: first_document_id,
                document_label: "Provider Alpha setup.md".to_string(),
                excerpt: "Alpha setup package is alpha-adapter.pkg.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Alpha setup package is alpha-adapter.pkg.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: second_document_id,
                document_label: "Provider Alpha appendix.md".to_string(),
                excerpt: "Alpha appendix parameter is retryTimeout.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: "Alpha appendix parameter is retryTimeout.".to_string(),
            },
        ],
        &[],
    );

    assert!(!context.contains("Focused grounded document"), "{context}");
    assert!(context.contains("alpha-adapter.pkg"));
    assert!(context.contains("retryTimeout"));
}

#[test]
fn build_authoritative_answer_context_keeps_typed_runtime_graph_evidence() {
    let document_id = Uuid::now_v7();
    let context = build_authoritative_answer_context(
        "Which rare graph value belongs to Project Omega?",
        &generic_query_ir(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "project_omega.md".to_string(),
            excerpt: "Rare runtime evidence: omega.flag=enabled.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
            score: Some(0.9),
            source_text: "Rare runtime evidence: omega.flag=enabled.".to_string(),
        }],
        &[],
    );

    assert!(context.contains("scope=graph_evidence"));
    assert!(context.contains("omega.flag=enabled"));
    assert!(!context.contains("[graph-evidence target=\"Project Omega\"]"));
    assert!(!context.contains("[graph-node] Project Omega"));
}

#[test]
fn build_authoritative_answer_context_keeps_unhydrated_graph_evidence_lines() {
    let context = build_authoritative_answer_context(
        "Which rare graph value belongs to Project Omega?",
        &generic_query_ir(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        &[
            "[graph-evidence target=\"Project Omega\"]".to_string(),
            "Rare runtime evidence: omega.flag=enabled.".to_string(),
        ],
    );

    assert!(context.contains("Retrieved graph evidence"));
    assert!(context.contains("[graph-evidence target=\"Project Omega\"]"));
    assert!(context.contains("omega.flag=enabled"));
}

#[test]
fn apply_runtime_chunk_overlays_preserves_shorter_authoritative_graph_evidence() {
    let chunk_id = Uuid::now_v7();
    let mut chunks = vec![RuntimeMatchedChunk {
        chunk_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "project_omega.md".to_string(),
        excerpt: "Long generic source excerpt before graph overlay.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.2),
        source_text:
            "Long generic source text that happens to be much longer than the graph evidence."
                .to_string(),
    }];
    let runtime_chunks = vec![RuntimeMatchedChunk {
        chunk_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: chunks[0].document_id,
        document_label: chunks[0].document_label.clone(),
        excerpt: "omega.flag=enabled".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
        score: Some(0.9),
        source_text: "omega.flag=enabled".to_string(),
    }];

    apply_runtime_chunk_overlays(&mut chunks, &runtime_chunks);

    assert_eq!(
        chunks[0].score_kind,
        crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence
    );
    assert_eq!(chunks[0].score, Some(0.9));
    assert_eq!(chunks[0].source_text, "omega.flag=enabled");
    assert_eq!(chunks[0].excerpt, "omega.flag=enabled");
}

#[test]
fn merge_runtime_context_chunks_keeps_companions_missing_from_bundle_refs() {
    let mut chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "overview.md".to_string(),
        excerpt: "General overview.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.2),
        source_text: "General overview.".to_string(),
    }];
    let companion_id = Uuid::now_v7();
    let runtime_chunks = vec![RuntimeMatchedChunk {
        chunk_id: companion_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 7,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "settings.md".to_string(),
        excerpt: "terminalErrorCodes = 101, 202".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(0.9),
        source_text: "[Main]\nterminalErrorCodes = 101, 202".to_string(),
    }];

    merge_runtime_context_chunks(&mut chunks, &runtime_chunks);

    assert!(chunks.iter().any(|chunk| chunk.chunk_id == companion_id));
    assert_eq!(chunks.len(), 2);
}

#[test]
fn build_authoritative_answer_context_promotes_query_matching_prepared_segments_before_truncation()
{
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut structured_blocks = (0..20)
        .map(|ordinal| KnowledgeStructuredBlockRow {
            ordinal,
            normalized_text: format!("Generic filler segment {ordinal} for a broad manual."),
            text: format!("Generic filler segment {ordinal} for a broad manual."),
            heading_trail: vec!["Manual overview".to_string()],
            section_path: vec!["manual".to_string(), "overview".to_string()],
            ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
        })
        .collect::<Vec<_>>();
    structured_blocks.push(KnowledgeStructuredBlockRow {
        ordinal: 30,
        normalized_text:
            "Deferred ticket restoration covers regulated product category checks for controlled footwear."
                .to_string(),
        text:
            "Deferred ticket restoration covers regulated product category checks for controlled footwear."
                .to_string(),
        heading_trail: vec![
            "Deferred ticket".to_string(),
            "Regulated product category".to_string(),
            "Code verification rule".to_string(),
        ],
        section_path: vec![
            "deferred-ticket".to_string(),
            "regulated-product-category".to_string(),
            "code-verification-rule".to_string(),
        ],
        ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
    });

    let mut query_ir = generic_query_ir();
    query_ir.document_focus = Some(DocumentHint {
        hint: "deferred ticket regulated product code verification".to_string(),
    });
    query_ir.target_entities = vec![
        EntityMention { label: "regulated product category".to_string(), role: EntityRole::Object },
        EntityMention { label: "code verification".to_string(), role: EntityRole::Object },
    ];

    let context = build_authoritative_answer_context(
        "For the deferred ticket section, which regulated product category is tied to code verification?",
        &query_ir,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks,
            technical_facts: Vec::new(),
        },
        &[],
        &[],
    );

    assert!(
        context.contains("Deferred ticket > Regulated product category > Code verification rule")
    );
    assert!(context.contains("controlled footwear"));
    assert!(!context.contains("Generic filler segment 15"));
}
