use super::*;

#[test]
fn verify_answer_accepts_semantic_web_and_knowledge_graph_targets() {
    let verification = verify_answer_against_canonical_evidence(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_accepts_method_path_literal_when_method_and_path_are_grounded() {
    let verification = verify_answer_against_canonical_evidence(
        "Which endpoints are needed?",
        "The endpoint is `GET /system/info`.",
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: vec![KnowledgeChunkRow {
                key: Uuid::now_v7().to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("paragraph".to_string()),
                content_text:
                    "Send a GET request to /system/info to fetch the current checkout server status"
                        .to_string(),
                normalized_text:
                    "Send a GET request to /system/info to fetch the current checkout server status"
                        .to_string(),
                span_start: Some(0),
                span_end: Some(80),
                token_count: Some(12),
                support_block_ids: vec![],
                section_path: vec![],
                heading_trail: vec![],
                literal_digest: None,
                chunk_state: "active".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: None,

                window_text: None,

                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            }],
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_accepts_literals_grounded_by_runtime_corpus() {
    let verification = verify_answer_against_canonical_evidence(
        "what is the logic in the code",
        "These files show backend logic in Rust. `query_repository.rs` stores `query_conversation`, `query_turn`, and `query_execution`. `audit_repository.rs` filters audit by `action_kind` and writes `iam.bootstrap.claim`.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
        &AssistantGroundingEvidence {
            verification_corpus: vec![
                r#"{"structuredContent":{"documentTitle":"query_repository.rs","content":"from query_conversation and query_turn joined to query_execution"},"isError":false}"#
                    .to_string(),
                r#"{"structuredContent":{"documentTitle":"audit_repository.rs","content":"append_audit_event filters by action_kind and writes iam.bootstrap.claim"},"isError":false}"#
                    .to_string(),
            ],
            document_references: Vec::new(),
        },
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_records_unsupported_literals_for_revision_guard() {
    let verification = verify_answer_against_canonical_evidence(
        "Which command is required?",
        "Run `democtl missing --flag`.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "Run democtl present --flag.",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["democtl missing --flag"]);
}

#[test]
fn verify_answer_accepts_quoted_literals_grounded_by_decoded_read_document_content() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            r#"surface_kind = "bootstrap" and result_kind = "succeeded""#.to_string(),
        ],
        document_references: Vec::new(),
    };

    let verification = verify_answer_against_canonical_evidence(
        "Which filters and events does audit_repository.rs handle?",
        "The file filters by `\"bootstrap\"` and `\"succeeded\"` in the literal-value examples.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
        &grounding,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_markdown_escaped_path_literals() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![r#"share path: \\ host\_name \scan-share"#.to_string()],
        document_references: Vec::new(),
    };

    let verification = verify_answer_against_canonical_evidence(
        "Which path is used?",
        r#"Use `\\host_name\scan-share`."#,
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
        &grounding,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_literals_grounded_by_html_entity_equivalent_context() {
    let verification = verify_answer_against_canonical_evidence(
        "Which example query was provided?",
        "The example was:\n\n```sql\nSELECT name, age\nFROM students\nWHERE age > 18\nORDER BY name ASC;\n```",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "Example:\nSELECT name, age\nFROM students\nWHERE age &gt; 18\nORDER BY name ASC;",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_named_decimal_and_hex_html_entities() {
    let verification = verify_answer_against_canonical_evidence(
        "Which literals were provided?",
        r#"Use `"alpha"`, `beta's`, and `/v1/items`."#,
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "Use &quot;alpha&quot;, beta&#39;s, and &#x2F;v1&#x2F;items.",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_does_not_decode_malformed_html_entity_without_semicolon() {
    let verification = verify_answer_against_canonical_evidence(
        "Which literal was provided?",
        "Use `AT&T`.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "Use AT&ampT.",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["AT&T"]);
}

#[test]
fn verify_answer_ignores_background_conflicts_when_grounded_literals_are_explicit() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_canonical_evidence(
        "Use the exact WSDL URL.",
        "Use `http://demo.local:8080/customer-profile/ws/customer-profile.wsdl`.",
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    canonical_value_exact: "http://demo.local:8080/customer-profile/ws/"
                        .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/"
                    ),
                    display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    canonical_value_text:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_exact:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                    ),
                    display_value:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[],
        "",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "conflicting_evidence"));
}

#[test]
fn verify_answer_marks_conflicting_when_exact_literal_question_stays_ambiguous() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_canonical_evidence(
        "What exact endpoint is described?",
        "The exact endpoint is described in the selected evidence.",
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "/system/info".to_string(),
                    canonical_value_exact: "/system/info".to_string(),
                    canonical_value_json: serde_json::json!("/system/info"),
                    display_value: "/system/info".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "/system/status".to_string(),
                    canonical_value_exact: "/system/status".to_string(),
                    canonical_value_json: serde_json::json!("/system/status"),
                    display_value: "/system/status".to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[],
        "",
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(verification.state, QueryVerificationState::Conflicting);
    assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
}

#[test]
fn enrich_query_candidate_summary_overwrites_canonical_reference_counts() {
    let enriched = enrich_query_candidate_summary(
        serde_json::json!({
            "finalChunkReferences": 1,
            "finalEntityReferences": 3,
            "finalRelationReferences": 2
        }),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: vec![
                sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
            ],
            structured_blocks: vec![sample_structured_block_row(
                Uuid::now_v7(),
                Uuid::now_v7(),
                Uuid::now_v7(),
            )],
            technical_facts: vec![
                sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
            ],
        },
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(enriched["finalChunkReferences"], serde_json::json!(2));
    assert_eq!(enriched["finalPreparedSegmentReferences"], serde_json::json!(1));
    assert_eq!(enriched["finalTechnicalFactReferences"], serde_json::json!(2));
    assert_eq!(enriched["finalEntityReferences"], serde_json::json!(3));
}

#[test]
fn enrich_query_assembly_diagnostics_emits_verification_and_graph_participation() {
    let diagnostics = enrich_query_assembly_diagnostics(
        serde_json::json!({
            "bundleId": Uuid::nil(),
        }),
        &RuntimeAnswerVerification {
            state: QueryVerificationState::Verified,
            warnings: vec![QueryVerificationWarning {
                code: "grounded".to_string(),
                message: "Answer is grounded.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
            unsupported_literals: Vec::new(),
        },
        &serde_json::json!({
            "finalChunkReferences": 2,
            "finalPreparedSegmentReferences": 4,
            "finalTechnicalFactReferences": 3,
            "finalEntityReferences": 5,
            "finalRelationReferences": 2
        }),
        &AssistantGroundingEvidence::default(),
    );

    assert_eq!(diagnostics["verificationState"], "verified");
    assert_eq!(diagnostics["verificationWarnings"][0]["code"], "grounded");
    assert_eq!(diagnostics["graphParticipation"]["entityReferenceCount"], 5);
    assert_eq!(diagnostics["graphParticipation"]["relationReferenceCount"], 2);
    assert_eq!(diagnostics["graphParticipation"]["graphBacked"], true);
    assert_eq!(diagnostics["structuredEvidence"]["preparedSegmentReferenceCount"], 4);
    assert_eq!(diagnostics["structuredEvidence"]["technicalFactReferenceCount"], 3);
    assert_eq!(diagnostics["structuredEvidence"]["chunkReferenceCount"], 2);
}

#[test]
fn selected_fact_ids_for_canonical_evidence_stays_bounded() {
    let selected_fact_id = Uuid::now_v7();
    let evidence_fact_id = Uuid::now_v7();
    let evidence_rows = vec![KnowledgeEvidenceRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        evidence_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: None,
        block_id: Some(Uuid::now_v7()),
        fact_id: Some(evidence_fact_id),
        span_start: None,
        span_end: None,
        quote_text: "GET /system/info".to_string(),
        literal_spans_json: json!([]),
        evidence_kind: "relation_fact_support".to_string(),
        extraction_method: "graph_extract".to_string(),
        confidence: Some(0.9),
        evidence_state: "active".to_string(),
        freshness_generation: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }];
    let chunk_supported_facts = (0..40)
        .map(|_| sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()))
        .collect::<Vec<_>>();

    let fact_ids = selected_fact_ids_for_canonical_evidence(
        &[selected_fact_id],
        &evidence_rows,
        &chunk_supported_facts,
    );
    assert_eq!(fact_ids.len(), 2);
    assert_eq!(fact_ids[0], selected_fact_id);
    assert_eq!(fact_ids[1], evidence_fact_id);
}

#[test]
fn apply_query_execution_warning_sets_typed_fields() {
    let mut diagnostics = RuntimeStructuredQueryDiagnostics {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        keywords: Vec::new(),
        high_level_keywords: Vec::new(),
        low_level_keywords: Vec::new(),
        top_k: 8,
        reference_counts: RuntimeStructuredQueryReferenceCounts {
            entity_count: 0,
            relationship_count: 0,
            chunk_count: 0,
            graph_node_count: 0,
            graph_edge_count: 0,
        },
        planning: crate::domains::query::QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords::default(),
            warnings: Vec::new(),
        },
        rerank: crate::domains::query::RerankMetadata {
            status: crate::domains::query::RerankStatus::Skipped,
            candidate_count: 0,
            reordered_count: None,
        },
        context_assembly: crate::domains::query::ContextAssemblyMetadata {
            status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
            warning: None,
        },
        grouped_references: Vec::new(),
        context_text: None,
        warning: None,
        warning_kind: None,
        library_summary: None,
    };
    apply_query_execution_warning(
        &mut diagnostics,
        Some(&RuntimeQueryWarning {
            warning: "Graph coverage is still converging.".to_string(),
            warning_kind: "partial_convergence",
        }),
    );

    assert_eq!(diagnostics.warning.as_deref(), Some("Graph coverage is still converging."));
    assert_eq!(diagnostics.warning_kind, Some("partial_convergence"));
}

#[test]
fn build_structured_query_diagnostics_emits_typed_response_shape() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec!["ironrag".to_string(), "graph".to_string()],
        high_level_keywords: vec!["ironrag".to_string()],
        low_level_keywords: vec!["graph".to_string()],
        entity_keywords: vec!["ironrag".to_string()],
        concept_keywords: vec!["graph".to_string()],
        top_k: 8,
        context_budget_chars: 6_000,
        hyde_recommended: false,
    };
    let bundle = RetrievalBundle {
        entities: vec![RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.91),
        }],
        relationships: vec![RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "mentions".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "spec.md".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "IronRAG".to_string(),
            summary: None,
            support_count: 1,
            score: Some(0.61),
        }],
        chunks: vec![RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG query runtime returns structured references.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.73),
            source_text: "IronRAG query runtime returns structured references.".to_string(),
        }],
    };
    let graph_index = QueryGraphIndex::empty();
    let enrichment = QueryExecutionEnrichment {
        planning: crate::domains::query::QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords {
                high_level: vec!["ironrag".to_string()],
                low_level: vec!["graph".to_string()],
            },
            warnings: Vec::new(),
        },
        rerank: crate::domains::query::RerankMetadata {
            status: crate::domains::query::RerankStatus::Skipped,
            candidate_count: 3,
            reordered_count: None,
        },
        context_assembly: crate::domains::query::ContextAssemblyMetadata {
            status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
            warning: None,
        },
        grouped_references: Vec::new(),
    };

    let diagnostics = build_structured_query_diagnostics(
        &plan,
        &bundle,
        &graph_index,
        &enrichment,
        true,
        "Bounded context",
    );

    assert_eq!(diagnostics.planned_mode, RuntimeQueryMode::Hybrid);
    assert_eq!(diagnostics.requested_mode, RuntimeQueryMode::Hybrid);
    assert_eq!(diagnostics.reference_counts.entity_count, 1);
    assert_eq!(diagnostics.reference_counts.relationship_count, 1);
    assert_eq!(diagnostics.reference_counts.chunk_count, 1);
    assert_eq!(diagnostics.reference_counts.graph_node_count, 0);
    assert_eq!(diagnostics.reference_counts.graph_edge_count, 0);
    assert_eq!(
        diagnostics.planning.intent_cache_status,
        crate::domains::query::QueryIntentCacheStatus::Miss
    );
    assert_eq!(
        diagnostics.context_assembly.status,
        crate::domains::query::ContextAssemblyStatus::BalancedMixed
    );
    assert!(diagnostics.grouped_references.is_empty());
    assert_eq!(diagnostics.context_text.as_deref(), Some("Bounded context"));
}
