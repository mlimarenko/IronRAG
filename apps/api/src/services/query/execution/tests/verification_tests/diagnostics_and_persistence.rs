use super::*;

#[test]
fn enrich_query_candidate_summary_overwrites_authoritative_reference_counts() {
    let enriched = enrich_query_candidate_summary(
        serde_json::json!({
            "finalChunkReferences": 1,
            "finalEntityReferences": 3,
            "finalRelationReferences": 2
        }),
        &AuthoritativeAnswerEvidence {
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
        QueryAnswerDisposition::FactualReady,
        &serde_json::json!({
            "finalChunkReferences": 2,
            "finalPreparedSegmentReferences": 4,
            "finalTechnicalFactReferences": 3,
            "finalEntityReferences": 5,
            "finalRelationReferences": 2
        }),
        &AssistantGroundingEvidence::default(),
    )
    .expect("typed verification diagnostics should serialize");

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
fn persisted_query_answer_outcome_round_trips_typed_clarification() {
    let clarification = QueryClarification {
        required: true,
        question: Some("Which neutral source should be used?".to_string()),
        answer_candidates: vec![QueryAnswerCandidate {
            label: "Neutral source".to_string(),
            kind: "document".to_string(),
            confidence: Some(0.75),
            provenance: QueryAnswerCandidateProvenance::default(),
        }],
    };
    let summary = attach_query_answer_outcome(
        serde_json::json!({"candidateCount": 1}),
        QueryAnswerDisposition::Clarification,
        &clarification,
        None,
    )
    .expect("typed clarification should persist");

    let (disposition, restored) =
        persisted_query_answer_outcome(&summary).expect("typed clarification should replay");

    assert_eq!(disposition, QueryAnswerDisposition::Clarification);
    assert_eq!(restored, clarification);
}

#[test]
fn persisted_query_answer_outcome_is_conservative_for_legacy_object() {
    let (disposition, clarification) = persisted_query_answer_outcome(&serde_json::json!({}))
        .expect("legacy object without additive fields should remain readable");

    assert_eq!(disposition, QueryAnswerDisposition::NonTerminal);
    assert_eq!(clarification, QueryClarification::default());
}

#[test]
fn persisted_query_answer_outcome_rejects_malformed_or_inconsistent_metadata() {
    let malformed_summary = serde_json::json!([]);
    let unknown_disposition = serde_json::json!({"answerDisposition": "unknown"});
    let missing_clarification = serde_json::json!({"answerDisposition": "clarification"});
    let contradictory_non_clarification = serde_json::json!({
        "answerDisposition": "factual_ready",
        "answerClarification": {
            "required": true,
            "question": "Choose one",
            "answerCandidates": []
        }
    });
    let inconsistent_write = attach_query_answer_outcome(
        serde_json::json!({}),
        QueryAnswerDisposition::FactualReady,
        &QueryClarification {
            required: true,
            question: Some("Choose one".to_string()),
            answer_candidates: Vec::new(),
        },
        None,
    );

    assert!(persisted_query_answer_outcome(&malformed_summary).is_err());
    assert_eq!(
        persisted_query_answer_outcome(&unknown_disposition)
            .expect("unknown historical disposition must fail closed"),
        (QueryAnswerDisposition::NonTerminal, QueryClarification::default()),
    );
    assert!(persisted_query_answer_outcome(&missing_clarification).is_err());
    assert!(persisted_query_answer_outcome(&contradictory_non_clarification).is_err());
    assert!(inconsistent_write.is_err());
}

#[test]
fn system_statement_answer_kind_is_persisted_and_detected() {
    let summary = attach_query_answer_outcome(
        serde_json::json!({"candidateCount": 0}),
        QueryAnswerDisposition::SafeFallback,
        &QueryClarification::default(),
        Some(crate::services::query::execution::answer_kind::AnswerKind::MissingExplicitDocument),
    )
    .expect("system statement outcome should persist");

    assert_eq!(summary["answerKind"], serde_json::json!("missing_explicit_document"));
    assert!(crate::services::query::execution::persisted_answer_is_system_statement(&summary));
    assert_eq!(
        persisted_query_answer_outcome(&summary).expect("additive answer kind stays readable"),
        (QueryAnswerDisposition::SafeFallback, QueryClarification::default()),
    );
}

#[test]
fn grounded_or_absent_answer_kind_is_not_a_system_statement() {
    let grounded = attach_query_answer_outcome(
        serde_json::json!({}),
        QueryAnswerDisposition::FactualReady,
        &QueryClarification::default(),
        None,
    )
    .expect("grounded outcome should persist");

    assert_eq!(grounded.get("answerKind"), None);
    assert!(!crate::services::query::execution::persisted_answer_is_system_statement(&grounded));
    assert!(!crate::services::query::execution::persisted_answer_is_system_statement(
        &serde_json::json!({"answerKind": "targeted_table_answer"})
    ));
}

#[test]
fn selected_fact_ids_for_authoritative_evidence_stays_bounded() {
    let selected_fact_id = Uuid::now_v7();
    let evidence_fact_id = Uuid::now_v7();
    let evidence_rows = vec![KnowledgeEvidenceRow {
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

    let fact_ids = selected_fact_ids_for_authoritative_evidence(
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
            semantic_rerank: None,
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
            summary: None,
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
            semantic_rerank: None,
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
