use super::*;
use serde_json::json;

#[test]
fn broad_procedure_variant_coverage_excludes_versioned_and_focused_requests() {
    let mut ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Procedure],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    };

    assert!(ir.requests_broad_procedure_variant_coverage());
    ir.target_types.push(QueryTargetKind::Version);
    assert!(!ir.requests_broad_procedure_variant_coverage());
    ir.target_types.retain(|target| *target != QueryTargetKind::Version);
    ir.document_focus = Some(DocumentHint { hint: "Sample setup guide".to_string() });
    assert!(!ir.requests_broad_procedure_variant_coverage());
}

#[test]
fn query_target_kind_preserves_stable_wire_values() {
    let kinds = [
        QueryTargetKind::Procedure,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::TableRow,
        QueryTargetKind::ErrorCode,
    ];

    assert_eq!(
        serde_json::to_value(kinds).expect("serialize target kinds"),
        json!(["procedure", "configuration_file", "table_row", "error_code"])
    );
}

#[test]
fn query_target_kind_rejects_unknown_compiler_value() {
    let error = serde_json::from_value::<QueryTargetKind>(json!("unregistered_target"))
        .expect_err("unknown target kind must fail closed");

    assert!(error.to_string().contains("unknown variant"));
}

#[test]
fn query_ir_schema_restricts_target_types_to_typed_enum() {
    let schema = query_ir_json_schema();
    let target_schema = &schema["properties"]["target_types"]["items"];

    assert_eq!(target_schema["type"], "string");
    assert!(target_schema["enum"].as_array().is_some_and(|values| {
        values.iter().any(|value| value == "procedure")
            && values.iter().any(|value| value == "configuration_file")
    }));
}

#[test]
fn minimal_descriptive_question_round_trips() {
    let ir = QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Ru,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Procedure],
        target_entities: vec![EntityMention {
            label: "payment module".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };
    let json = serde_json::to_value(&ir).unwrap();
    let parsed: QueryIR = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, ir);
}

#[test]
fn exact_literal_question_routes_strict() {
    let ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Endpoint],
        target_entities: vec![],
        literal_constraints: vec![LiteralSpan {
            text: "/system/info".to_string(),
            kind: LiteralKind::Path,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    };
    assert!(ir.is_exact_literal_technical());
    assert_eq!(ir.verification_level(), VerificationLevel::Strict);
    assert!(!ir.is_follow_up());
    assert_eq!(ir.exact_technical_literals().collect::<Vec<_>>(), vec!["/system/info"]);
}

#[test]
fn exact_technical_literals_filters_out_plain_alphabetic_spans() {
    // A query can carry both an exact-shaped literal (the fact-search
    // lane should target it) and a plain topic-echo word (which is not a
    // technical identifier and must not be sent to the exact-fact lane).
    let ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![
            LiteralSpan { text: "GET".to_string(), kind: LiteralKind::Identifier },
            LiteralSpan { text: "endpoint".to_string(), kind: LiteralKind::Identifier },
        ],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };

    assert_eq!(ir.exact_technical_literals().collect::<Vec<_>>(), vec!["GET"]);
    assert!(ir.has_exact_technical_literal());
    assert!(ir.is_exact_literal_technical());
}

#[test]
fn plain_alphabetic_identifier_literal_is_not_exact_technical() {
    let ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Concept],
        target_entities: vec![],
        literal_constraints: vec![LiteralSpan {
            text: "alpha".to_string(),
            kind: LiteralKind::Identifier,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.84,
    };

    assert!(!ir.has_exact_technical_literal());
    assert!(!ir.is_exact_literal_technical());
    assert!(ir.requests_source_coverage_context());
    assert_eq!(ir.verification_level(), VerificationLevel::Moderate);
}

#[test]
fn follow_up_detects_from_refs() {
    let ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Ru,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![UnresolvedRef {
            surface: "there".to_string(),
            kind: ConversationRefKind::Deictic,
        }],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.7,
    };
    assert!(ir.is_follow_up());
    assert_eq!(ir.verification_level(), VerificationLevel::Lenient);
}

#[test]
fn low_confidence_alone_does_not_trigger_clarification() {
    let ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.4,
    };
    assert!(!ir.should_request_clarification());
}

#[test]
fn explicit_clarification_reason_triggers_clarification() {
    let ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: Some(ClarificationSpec {
            reason: ClarificationReason::AmbiguousTooShort,
            suggestion: String::new(),
        }),
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };
    assert!(ir.should_request_clarification());
}

#[test]
fn broad_descriptive_ir_requests_source_coverage() {
    let ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.8,
    };
    assert!(ir.requests_source_coverage_context());
}

#[test]
fn source_slice_round_trips_as_typed_ir() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Record],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count: Some(20),
            filter: SourceSliceFilter::ReleaseMarker,
        }),
        retrieval_query: None,
        confidence: 0.86,
    };

    let json = serde_json::to_value(&ir).unwrap();
    let parsed: QueryIR = serde_json::from_value(json).unwrap();

    assert!(parsed.requests_source_slice_context());
    assert_eq!(parsed.source_slice, ir.source_slice);
}

#[test]
fn resolved_temporal_bounds_aggregates_min_start_and_max_end() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![
            TemporalConstraint {
                surface: "window-A".to_string(),
                start: Some("2026-03-01T00:00:00Z".to_string()),
                end: Some("2026-03-31T23:59:59Z".to_string()),
            },
            TemporalConstraint {
                surface: "window-B".to_string(),
                start: Some("2026-02-01T00:00:00Z".to_string()),
                end: Some("2026-02-28T23:59:59Z".to_string()),
            },
        ],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };

    let (start, end) = ir.resolved_temporal_bounds();
    let start = start.expect("min start");
    let end = end.expect("max end");
    assert_eq!(start.to_rfc3339(), "2026-02-01T00:00:00+00:00");
    assert_eq!(end.to_rfc3339(), "2026-03-31T23:59:59+00:00");
}

#[test]
fn resolved_temporal_bounds_returns_none_when_constraints_empty() {
    let ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.5,
    };

    assert_eq!(ir.resolved_temporal_bounds(), (None, None));
}

#[test]
fn resolved_temporal_bounds_returns_only_start_when_only_start_provided() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![TemporalConstraint {
            surface: "window-half-open".to_string(),
            start: Some("2026-04-01T00:00:00Z".to_string()),
            end: None,
        }],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.7,
    };
    let (start, end) = ir.resolved_temporal_bounds();
    assert!(start.is_some());
    assert_eq!(end, None);
    assert_eq!(start.unwrap().to_rfc3339(), "2026-04-01T00:00:00+00:00");
}

#[test]
fn resolved_temporal_bounds_returns_only_end_when_only_end_provided() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![TemporalConstraint {
            surface: "window-half-closed".to_string(),
            start: None,
            end: Some("2026-04-30T23:59:59Z".to_string()),
        }],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.7,
    };
    let (start, end) = ir.resolved_temporal_bounds();
    assert_eq!(start, None);
    assert!(end.is_some());
    assert_eq!(end.unwrap().to_rfc3339(), "2026-04-30T23:59:59+00:00");
}

#[test]
fn resolved_temporal_bounds_aggregates_parseable_when_mixed_with_unparseable() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![
            TemporalConstraint {
                surface: "window-good".to_string(),
                start: Some("2026-05-01T00:00:00Z".to_string()),
                end: Some("2026-05-31T23:59:59Z".to_string()),
            },
            TemporalConstraint {
                surface: "window-bad".to_string(),
                start: Some("not a date".to_string()),
                end: Some("also not a date".to_string()),
            },
        ],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };
    let (start, end) = ir.resolved_temporal_bounds();
    // Only the parseable surface contributes; the broken one is silently skipped.
    assert_eq!(start.unwrap().to_rfc3339(), "2026-05-01T00:00:00+00:00");
    assert_eq!(end.unwrap().to_rfc3339(), "2026-05-31T23:59:59+00:00");
}

#[test]
fn resolved_temporal_bounds_skips_unparseable_rfc3339_surfaces() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![],
        temporal_constraints: vec![TemporalConstraint {
            surface: "later".to_string(),
            start: Some("not a date".to_string()),
            end: None,
        }],
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.4,
    };

    assert_eq!(ir.resolved_temporal_bounds(), (None, None));
}

#[test]
fn temporal_constraints_round_trip_as_typed_ir() {
    let ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Record],
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
        retrieval_query: None,
        confidence: 0.86,
    };

    let json = serde_json::to_value(&ir).unwrap();
    let parsed: QueryIR = serde_json::from_value(json).unwrap();

    assert_eq!(parsed.temporal_constraints, ir.temporal_constraints);
}

#[test]
fn exact_and_follow_up_ir_do_not_request_source_coverage() {
    let exact_ir = QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![],
        target_entities: vec![],
        literal_constraints: vec![LiteralSpan {
            text: "DATABASE_URL".to_string(),
            kind: LiteralKind::Identifier,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: vec![],
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.8,
    };
    let follow_up_ir = QueryIR {
        act: QueryAct::Describe,
        conversation_refs: vec![UnresolvedRef {
            surface: "that".to_string(),
            kind: ConversationRefKind::Deictic,
        }],
        ..exact_ir.clone()
    };
    assert!(!exact_ir.requests_source_coverage_context());
    assert!(!follow_up_ir.requests_source_coverage_context());
}

#[test]
fn literal_kind_infer_keeps_plain_words_out_of_identifier_routing() {
    assert_eq!(LiteralKind::infer("alpha"), LiteralKind::Other);
    assert_eq!(LiteralKind::infer("Настройки"), LiteralKind::Other);
    assert_eq!(LiteralKind::infer("callbackUrl"), LiteralKind::Identifier);
    assert_eq!(LiteralKind::infer("DATABASE_URL"), LiteralKind::Identifier);
    assert_eq!(LiteralKind::infer("Настройка_2"), LiteralKind::Identifier);
}

#[test]
fn schema_has_all_top_level_properties() {
    let schema = query_ir_json_schema();
    let required = schema["required"].as_array().unwrap();
    for field in [
        "act",
        "scope",
        "language",
        "target_types",
        "target_entities",
        "literal_constraints",
        "temporal_constraints",
        "comparison",
        "document_focus",
        "conversation_refs",
        "needs_clarification",
        "source_slice",
        "retrieval_query",
        "confidence",
    ] {
        assert!(required.iter().any(|value| value == field), "schema should require `{field}`");
    }
}
