use super::*;

#[test]
fn build_lexical_queries_keeps_broader_unique_query_set() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Mix,
        planned_mode: RuntimeQueryMode::Mix,
        intent_profile: QueryIntentProfile { exact_literal_technical: true, ..Default::default() },
        keywords: vec![
            "program".to_string(),
            "profile".to_string(),
            "discount".to_string(),
            "tier".to_string(),
        ],
        high_level_keywords: vec!["program".to_string(), "profile".to_string()],
        low_level_keywords: vec!["discount".to_string(), "tier".to_string()],
        entity_keywords: vec![],
        concept_keywords: vec![],
        top_k: 48,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };

    let question = "If the agent needs the current checkout server status and the rewards service accounts list separately, which two endpoints does it need?";
    let queries = build_lexical_queries(question, &plan, &[], None);

    // Raw question goes first — Arango's full-text analyser already
    // splits it into relevant tokens and the broader phrasing is the
    // highest-signal single query we can dispatch. The combined
    // keyword phrase ("program profile discount tier") is still
    // emitted, but one slot later.
    assert_eq!(queries[0], question);
    assert!(queries.contains(&"program profile discount tier".to_string()));
    // Without QueryIR, retrieval-time segmentation still carries the
    // identifying terms ("current checkout server status" and
    // "rewards service accounts list") and Arango's analyser strips
    // the framing tokens downstream.
    assert!(
        queries.iter().any(|query| query.contains("current checkout server status")),
        "segments should include the checkout clause: {queries:?}"
    );
    assert!(
        queries.iter().any(|query| query.contains("rewards service accounts list")),
        "segments should include the rewards clause: {queries:?}"
    );
    assert!(queries.contains(&"program profile".to_string()));
    assert!(queries.contains(&"discount tier".to_string()));
    assert!(queries.contains(&"program".to_string()));
    // Budget-capped: with all three question clauses emitted as
    // separate lexical queries (retrieval-stage segmentation is
    // IR-blind), the final single-keyword slot goes to the first
    // plan keyword rather than further ones.
}

#[test]
fn build_lexical_queries_does_not_expand_role_targets_from_raw_language() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile::default(),
        keywords: Vec::new(),
        high_level_keywords: Vec::new(),
        low_level_keywords: Vec::new(),
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        top_k: 8,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };

    let question = "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?";
    let queries = build_lexical_queries(question, &plan, &[], None);

    assert_eq!(queries[0], question);
    assert!(!queries.contains(&"retrieval-augmented generation".to_string()));
    assert!(!queries.contains(&"vector database".to_string()));
}

#[test]
fn build_lexical_queries_uses_query_ir_focus_spans_before_broad_keywords() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec![
            "configure".to_string(),
            "scan".to_string(),
            "folder".to_string(),
            "settings".to_string(),
        ],
        high_level_keywords: vec!["configure".to_string(), "scan".to_string()],
        low_level_keywords: vec!["folder".to_string(), "settings".to_string()],
        entity_keywords: vec![],
        concept_keywords: vec![],
        top_k: 8,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };
    let query_ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["protocol".to_string()],
        target_entities: vec![
            EntityMention {
                label: "scan folder through RareProtocol".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
        ],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(DocumentHint {
            hint: "RareProtocol scan-folder setup commands".to_string(),
        }),
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };

    let question = "How do I configure folder scanning through RareProtocol?";
    let focus_queries = query_ir_lexical_focus_queries(&query_ir);
    let queries = build_lexical_queries(question, &plan, &focus_queries, Some(&query_ir));

    assert_eq!(queries[0], question);
    assert_eq!(queries[1], "scan folder through RareProtocol RareProtocol daemon");
    assert_eq!(queries[2], "scan folder through RareProtocol");
    assert_eq!(queries[3], "RareProtocol daemon");
    assert!(!queries.contains(&"RareProtocol scan-folder setup commands".to_string()));
    assert!(queries.contains(&"configure scan folder settings".to_string()));
}

#[test]
fn query_ir_focus_search_queries_use_typed_focus_before_broad_question() {
    let focus_queries = vec![
        "RareProtocol scan-folder setup commands".to_string(),
        "RareProtocol daemon".to_string(),
    ];
    let question = "How do I configure folder scanning through RareProtocol?";

    let queries = query_ir_focus_search_queries(question, &focus_queries);

    assert_eq!(queries[0], "RareProtocol scan-folder setup commands");
    assert_eq!(queries[1], "RareProtocol daemon");
    assert!(!queries.contains(&question.to_string()));
}

#[test]
fn build_graph_evidence_text_queries_keeps_raw_question_recall_anchor() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile { exact_literal_technical: true, ..Default::default() },
        keywords: vec![
            "configure".to_string(),
            "alpha".to_string(),
            "9407".to_string(),
            "endpoint".to_string(),
        ],
        high_level_keywords: vec!["configure".to_string(), "alpha".to_string()],
        low_level_keywords: vec!["9407".to_string(), "endpoint".to_string()],
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        top_k: 8,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };
    let query_ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["setting".to_string()],
        target_entities: vec![EntityMention {
            label: "Alpha endpoint 9407".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };
    let question =
        "Can you explain every surrounding detail before telling me how Alpha endpoint 9407 works?";
    let focus_queries = query_ir_lexical_focus_queries(&query_ir);
    let queries =
        build_graph_evidence_text_queries(question, &plan, &focus_queries, Some(&query_ir));

    assert_eq!(queries.first().map(String::as_str), Some(question));
    assert_eq!(queries.get(1).map(String::as_str), Some("Alpha endpoint 9407"));
    assert!(queries.contains(&"configure alpha 9407 endpoint".to_string()));
}

#[test]
fn query_ir_focus_queries_start_with_adjacent_typed_compounds() {
    let query_ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["endpoint".to_string()],
        target_entities: vec![EntityMention {
            label: "Beta Display".to_string(),
            role: EntityRole::Object,
        }],
        literal_constraints: vec![
            LiteralSpan { text: "Alpha Report".to_string(), kind: LiteralKind::Other },
            LiteralSpan { text: "Mono Font".to_string(), kind: LiteralKind::Other },
        ],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };

    let focus_queries = query_ir_lexical_focus_queries(&query_ir);

    assert_eq!(focus_queries.first().map(String::as_str), Some("Alpha Report Mono Font"));
    assert_eq!(focus_queries.get(1).map(String::as_str), Some("Mono Font Beta Display"));
    assert!(focus_queries.contains(&"Alpha Report".to_string()));
}

#[test]
fn query_ir_focus_queries_order_compounds_by_structural_specificity() {
    let query_ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["endpoint".to_string()],
        target_entities: Vec::new(),
        literal_constraints: vec![
            LiteralSpan { text: "Alpha".to_string(), kind: LiteralKind::Other },
            LiteralSpan { text: "Beta Report".to_string(), kind: LiteralKind::Other },
            LiteralSpan { text: "Mono Font".to_string(), kind: LiteralKind::Other },
            LiteralSpan { text: "Current Window".to_string(), kind: LiteralKind::Other },
        ],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };

    let focus_queries = query_ir_lexical_focus_queries(&query_ir);

    assert_eq!(focus_queries.first().map(String::as_str), Some("Mono Font Current Window"));
    assert!(
        focus_queries
            .iter()
            .position(|query| query == "Beta Report Mono Font")
            .is_some_and(|position| position < 2)
    );
}

#[test]
fn query_ir_focus_queries_include_iso_temporal_prefixes() {
    let query_ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["record".to_string()],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![TemporalConstraint {
            surface: "period 2026-03".to_string(),
            start: Some("2026-03-01T00:00:00Z".to_string()),
            end: Some("2026-04-01T00:00:00Z".to_string()),
        }],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };

    let focus_queries = query_ir_lexical_focus_queries(&query_ir);

    assert!(focus_queries.contains(&"2026-03".to_string()));
}

#[test]
fn query_ir_focus_queries_prefer_day_prefix_for_single_day_ranges() {
    let query_ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["record".to_string()],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![TemporalConstraint {
            surface: "period 2026-03-14".to_string(),
            start: Some("2026-03-14T00:00:00Z".to_string()),
            end: Some("2026-03-15T00:00:00Z".to_string()),
        }],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        confidence: 0.8,
    };

    let focus_queries = query_ir_lexical_focus_queries(&query_ir);

    assert_eq!(focus_queries.first().map(String::as_str), Some("2026-03-14"));
}

#[test]
fn apply_rerank_outcome_reorders_bundle_before_final_truncation() {
    let entity_a = Uuid::now_v7();
    let entity_b = Uuid::now_v7();
    let chunk_a = Uuid::now_v7();
    let chunk_b = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: vec![
            RuntimeMatchedEntity {
                node_id: entity_a,
                label: "Alpha".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: entity_b,
                label: "Budget".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.4),
            },
        ],
        relationships: Vec::new(),
        chunks: vec![
            RuntimeMatchedChunk {
                chunk_id: chunk_a,
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: Uuid::now_v7(),
                document_label: "alpha.md".to_string(),
                excerpt: "Alpha excerpt".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.8),
                source_text: "Alpha excerpt".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: chunk_b,
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: Uuid::now_v7(),
                document_label: "budget.md".to_string(),
                excerpt: "Budget approval memo".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.2),
                source_text: "Budget approval memo".to_string(),
            },
        ],
    };

    apply_rerank_outcome(
        &mut bundle,
        &RerankOutcome {
            entities: vec![entity_b.to_string(), entity_a.to_string()],
            relationships: Vec::new(),
            chunks: vec![chunk_b.to_string(), chunk_a.to_string()],
            metadata: crate::domains::query::RerankMetadata {
                status: crate::domains::query::RerankStatus::Applied,
                candidate_count: 4,
                reordered_count: Some(4),
            },
        },
    );
    truncate_bundle(&mut bundle, 1);

    assert_eq!(bundle.entities[0].node_id, entity_b);
    assert_eq!(bundle.chunks[0].chunk_id, chunk_b);
}

#[test]
fn maps_query_graph_status_from_library_generation() {
    let ready_generation = KnowledgeLibraryGenerationRow {
        key: "ready".to_string(),
        arango_id: None,
        arango_rev: None,
        generation_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        active_text_generation: 3,
        active_vector_generation: 5,
        active_graph_generation: 7,
        degraded_state: "ready".to_string(),
        updated_at: chrono::Utc::now(),
    };
    let degraded_generation = KnowledgeLibraryGenerationRow {
        degraded_state: "degraded".to_string(),
        ..ready_generation.clone()
    };
    let empty_generation = KnowledgeLibraryGenerationRow {
        active_graph_generation: 0,
        degraded_state: "degraded".to_string(),
        ..ready_generation
    };

    assert_eq!(query_graph_status(Some(&degraded_generation)), "partial");
    assert_eq!(query_graph_status(Some(&empty_generation)), "empty");
    assert_eq!(query_graph_status(None), "empty");
}
