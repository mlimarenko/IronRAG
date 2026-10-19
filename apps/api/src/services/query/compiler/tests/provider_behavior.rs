use super::*;
use crate::domains::query_ir::{
    ComparisonSpec, ConversationRefKind, DocumentHint, EntityMention, EntityRole, QueryLanguage,
    SourceSliceDirection, TemporalConstraint, UnresolvedRef,
};

/// The fields every QueryIR carries but these tests never vary, so a test can
/// state only the part it is actually about. `overrides` is merged over the
/// skeleton, which also lets a test add a field the skeleton omits.
fn ir_json_with(overrides: serde_json::Value) -> String {
    let mut ir = json!({
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null
    });
    let object = ir.as_object_mut().expect("skeleton is an object");
    for (key, value) in overrides.as_object().expect("overrides is an object") {
        object.insert(key.clone(), value.clone());
    }
    ir.to_string()
}

/// A `compare` act carrying no comparison operands: structurally well-formed
/// JSON that fails typed validation (`CompareWithoutComparison`). This is the
/// shape observed from a live provider.
fn unusable_comparison_ir_json() -> String {
    ir_json_with(json!({
        "act": "compare",
        "scope": "multi_document",
        "language": "en",
        "target_types": [],
        "target_entities": [],
        "retrieval_query": "compare",
        "confidence": 0.8
    }))
}

/// A QueryIR that passes typed validation for
/// "how do I configure the payment module?".
fn usable_configure_ir_json() -> String {
    ir_json_with(json!({
        "act": "configure_how",
        "scope": "single_document",
        "language": "ru",
        "target_types": ["procedure"],
        "target_entities": [{"label": "payment module", "role": "subject"}],
        "confidence": 0.9
    }))
}

#[tokio::test]
async fn compiles_descriptive_question_into_ir() {
    let ir_json = json!({
        "act": "configure_how",
        "scope": "single_document",
        "language": "ru",
        "target_types": ["procedure"],
        "target_entities": [{"label": "payment module", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.9
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "how do I configure the payment module?", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert_eq!(outcome.ir.language, QueryLanguage::Ru);
    assert_eq!(outcome.verification_level(), VerificationLevel::Lenient);
    let request = gateway.last_request.lock().unwrap().clone().unwrap();
    assert_eq!(request.provider_kind, "openai");
    assert_eq!(request.model_name, "gpt-5.4-nano");
    assert!(request.response_format.is_some(), "structured response format must be attached");
    assert!(request.prompt.contains("how do I configure the payment module?"));
}

#[tokio::test]
async fn preserves_valid_provider_ir_without_post_llm_semantic_rewrite() {
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["procedure"],
        "target_entities": [{"label": "Subject Alpha", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "Guide A"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "retrieval_query": "Subject Alpha setup",
        "confidence": 0.25
    })
    .to_string();
    let expected: QueryIR = serde_json::from_str(&ir_json).expect("fixture QueryIR");
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));

    let outcome = QueryCompilerService
        .compile_with_gateway(&gateway, &sample_binding(), "Use Guide A for Subject Alpha", &[])
        .await
        .expect("valid provider IR");

    assert_eq!(outcome.ir, expected);
}

#[tokio::test]
async fn rejects_stateless_conversation_dependent_ir_instead_of_rewriting_it() {
    let ir_json = json!({
        "act": "follow_up",
        "scope": "single_document",
        "language": "en",
        "target_types": [],
        "target_entities": [],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [{"surface": "that", "kind": "deictic"}],
        "needs_clarification": "anaphora_unresolved",
        "source_slice": null,
        "retrieval_query": "that",
        "confidence": 0.35
    })
    .to_string();
    let gateway = StubGateway::repeating(Ok(chat_response_with(&ir_json)));

    let error = QueryCompilerService
        .compile_with_gateway(&gateway, &sample_binding(), "that", &[])
        .await
        .expect_err("stateless conversation dependency must fail closed");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}

#[tokio::test]
async fn rejects_ungrounded_provider_entity_instead_of_fuzzy_repairing_it() {
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["artifact"],
        "target_entities": [{"label": "Subject Alpah", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "retrieval_query": "Subject Alpha",
        "confidence": 0.8
    })
    .to_string();
    let gateway = StubGateway::repeating(Ok(chat_response_with(&ir_json)));

    let error = QueryCompilerService
        .compile_with_gateway(&gateway, &sample_binding(), "Describe Subject Alpha", &[])
        .await
        .expect_err("ungrounded entity must fail closed");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}

#[tokio::test]
async fn retries_once_when_the_model_returns_an_unusable_ir() {
    // A QueryIR that fails typed validation is a formatting slip, not a verdict
    // on the question: the same prompt usually compiles on the next attempt.
    // Without the retry a single slip terminates the turn, and because every
    // provider transient escalates to HTTP 500 the caller sees a server error
    // for a recoverable event. Observed on the golden rig as
    // InvalidTypedIr(CompareWithoutComparison).
    let unusable = unusable_comparison_ir_json();
    let usable = usable_configure_ir_json();
    let gateway = StubGateway::scripted(vec![
        Ok(chat_response_with(&unusable)),
        Ok(chat_response_with(&usable)),
    ]);

    let outcome = QueryCompilerService
        .compile_with_gateway(
            &gateway,
            &sample_binding(),
            "how do I configure the payment module?",
            &[],
        )
        .await
        .expect("a second attempt that compiles must succeed");

    assert_eq!(gateway.calls(), 2, "the unusable IR must cost exactly one extra attempt");
    assert!(!outcome.served_from_cache);
}

#[tokio::test]
async fn stops_after_one_retry_when_the_model_keeps_failing() {
    // The bound matters as much as the retry: a model failing systematically
    // must not burn paid calls turning a fast error into a slow one.
    let unusable = unusable_comparison_ir_json();
    let gateway = StubGateway::repeating(Ok(chat_response_with(&unusable)));

    let error = QueryCompilerService
        .compile_with_gateway(&gateway, &sample_binding(), "compare", &[])
        .await
        .expect_err("a model that fails every attempt must still fail the compile");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
    assert_eq!(gateway.calls(), 2, "exactly one retry, not an unbounded loop");
}

#[tokio::test]
async fn rejects_structurally_invalid_provider_ir_in_release_path() {
    let ir_json = unusable_comparison_ir_json();
    let gateway = StubGateway::repeating(Ok(chat_response_with(&ir_json)));

    let error = QueryCompilerService
        .compile_with_gateway(&gateway, &sample_binding(), "compare", &[])
        .await
        .expect_err("invalid typed IR must fail in every build profile");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}

#[test]
fn request_validation_rejects_follow_up_retrieval_query_that_drops_prior_target() {
    let history = vec![CompileHistoryTurn {
        role: "user".to_string(),
        content: "Configure Subject Alpha with Variant A or Variant B.".to_string(),
    }];
    let mut ir = compiled_ir();
    ir.act = QueryAct::ConfigureHow;
    ir.target_entities = vec![
        EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject },
        EntityMention { label: "Variant B".to_string(), role: EntityRole::Object },
    ];
    ir.retrieval_query = Some("Variant B".to_string());

    assert_eq!(
        validate_compiled_ir_for_request("Variant B", &history, &ir),
        Err(CompileRequestValidationError::IncompleteRetrievalQuery)
    );

    ir.retrieval_query = Some("Subject Alpha".to_string());
    assert_eq!(
        validate_compiled_ir_for_request("Variant B", &history, &ir),
        Err(CompileRequestValidationError::IncompleteRetrievalQuery)
    );

    ir.retrieval_query = Some("Subject Alpha Variant B".to_string());
    assert_eq!(validate_compiled_ir_for_request("Variant B", &history, &ir), Ok(()));
}

#[test]
fn request_validation_accepts_exact_surfaces_from_question_and_history() {
    let history = vec![CompileHistoryTurn {
        role: "assistant".to_string(),
        content: "EntityH rightH refH".to_string(),
    }];
    let mut ir = compiled_ir();
    ir.target_entities = vec![
        EntityMention { label: "EntityQ".to_string(), role: EntityRole::Subject },
        EntityMention { label: "EntityH".to_string(), role: EntityRole::Object },
    ];
    ir.literal_constraints =
        vec![LiteralSpan { text: "LIT_Q".to_string(), kind: LiteralKind::Identifier }];
    ir.temporal_constraints = vec![TemporalConstraint {
        surface: "2026-03".to_string(),
        start: Some("2026-03-01T00:00:00Z".to_string()),
        end: Some("2026-04-01T00:00:00Z".to_string()),
    }];
    ir.document_focus = Some(DocumentHint { hint: "GuideQ".to_string() });
    ir.conversation_refs =
        vec![UnresolvedRef { surface: "refH".to_string(), kind: ConversationRefKind::Deictic }];
    ir.comparison = Some(ComparisonSpec {
        a: Some("leftQ".to_string()),
        b: Some("rightH".to_string()),
        dimension: "fixture".to_string(),
    });
    ir.retrieval_query = Some("EntityQ EntityH LIT_Q 2026-03 GuideQ leftQ rightH".to_string());

    assert_eq!(
        validate_compiled_ir_for_request("EntityQ LIT_Q 2026-03 GuideQ leftQ", &history, &ir,),
        Ok(())
    );
}

#[test]
fn request_validation_rejects_every_ungrounded_source_field() {
    let history =
        vec![CompileHistoryTurn { role: "user".to_string(), content: "prior source".to_string() }];

    let mut entity = compiled_ir();
    entity.target_entities =
        vec![EntityMention { label: "missing entity".to_string(), role: EntityRole::Subject }];

    let mut literal = compiled_ir();
    literal.literal_constraints =
        vec![LiteralSpan { text: "missing_literal".to_string(), kind: LiteralKind::Identifier }];

    let mut temporal = compiled_ir();
    temporal.temporal_constraints = vec![TemporalConstraint {
        surface: "missing temporal".to_string(),
        start: None,
        end: None,
    }];

    let mut document = compiled_ir();
    document.document_focus = Some(DocumentHint { hint: "missing document".to_string() });

    let mut reference = compiled_ir();
    reference.conversation_refs = vec![UnresolvedRef {
        surface: "missing reference".to_string(),
        kind: ConversationRefKind::Deictic,
    }];

    let mut comparison = compiled_ir();
    comparison.comparison = Some(ComparisonSpec {
        a: Some("missing operand".to_string()),
        b: None,
        dimension: "fixture".to_string(),
    });

    for (ir, expected) in [
        (entity, CompileRequestValidationError::UngroundedTargetEntity),
        (literal, CompileRequestValidationError::UngroundedLiteral),
        (temporal, CompileRequestValidationError::UngroundedTemporalSurface),
        (document, CompileRequestValidationError::UngroundedDocumentFocus),
        (reference, CompileRequestValidationError::UngroundedConversationReference),
        (comparison, CompileRequestValidationError::UngroundedComparisonOperand),
    ] {
        assert_eq!(
            validate_compiled_ir_for_request("current source", &history, &ir),
            Err(expected)
        );
    }
}

#[tokio::test]
async fn rejects_unknown_provider_target_kind_without_reclassification() {
    let ir_json = json!({
        "act": "describe",
        "scope": "multi_document",
        "language": "auto",
        "target_types": ["fixture_kind"],
        "target_entities": [],
        "literal_constraints": [
            {"text": "urn:fixture:item-17", "kind": "identifier"}
        ],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.82
    })
    .to_string();
    let gateway = StubGateway::repeating(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();
    let question = "∆ ⟦urn:fixture:item-17⟧";

    let error = service
        .compile_with_gateway(&gateway, &binding, question, &[])
        .await
        .expect_err("unknown target kind must fail the compile boundary");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}

#[tokio::test]
async fn low_confidence_plain_question_without_configuration_surface_stays_unfocused() {
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": [],
        "target_entities": [],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.25
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "Alpha Suite X9 2.3.4 2.3.5", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.act, QueryAct::Describe);
    assert!(outcome.ir.target_types.is_empty());
}

#[tokio::test]
async fn preserves_single_document_focus_when_user_quotes_a_technical_literal() {
    // The typed provider output is accepted unchanged because every
    // source-bearing field is present verbatim in the request.
    let ir_json = json!({
        "act": "configure_how",
        "scope": "single_document",
        "language": "en",
        "target_types": ["procedure"],
        "target_entities": [{"label": "S", "role": "subject"}],
        "literal_constraints": [{"text": "/v2/pay", "kind": "path"}],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "S"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.9
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "how do I configure S using /v2/pay", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert!(outcome.ir.document_focus.is_some());
}

#[tokio::test]
async fn preserves_single_document_focus_when_focus_hint_is_a_filename() {
    // Exact source grounding validates the filename without inferring
    // whether the filename should or should not select one document.
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["procedure"],
        "target_entities": [{"label": "S", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "S.conf"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.9
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "describe S.conf", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert_eq!(outcome.ir.document_focus.as_ref().map(|focus| focus.hint.as_str()), Some("S.conf"));
}

#[tokio::test]
async fn preserves_single_document_focus_when_target_type_is_document() {
    // Closed target kinds are trusted from the typed compiler output;
    // source-bearing labels and focus hints still require exact grounding.
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["document"],
        "target_entities": [{"label": "S", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "S"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.9
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "describe the S document", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert!(outcome.ir.document_focus.is_some());
}

#[tokio::test]
async fn keeps_low_confidence_single_document_focus_untouched() {
    // Validation is profile- and confidence-independent: valid typed IR is
    // never rewritten after the provider returns it.
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["procedure"],
        "target_entities": [{"label": "S", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "S"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.4
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome =
        service.compile_with_gateway(&gateway, &binding, "S", &[]).await.expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert!(outcome.ir.document_focus.is_some());
}

#[tokio::test]
async fn does_not_infer_release_tail_slice_from_ontology_words() {
    let ir_json = json!({
        "act": "enumerate",
        "scope": "single_document",
        "language": "en",
        "target_types": ["release", "version"],
        "target_entities": [],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.95
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "list the latest release records", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert!(outcome.ir.source_slice.is_none());
}

#[tokio::test]
async fn preserves_compiler_emitted_twenty_item_release_slice() {
    let ir_json = json!({
        "act": "enumerate",
        "scope": "single_document",
        "language": "en",
        "target_types": ["release", "version"],
        "target_entities": [],
        "literal_constraints": [{"text": "20", "kind": "numeric_code"}],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": {"direction": "tail", "count": 20, "filter": "release_marker"},
        "confidence": 0.95
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "list the latest 20 release records", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.source_slice.as_ref().and_then(|slice| slice.count), Some(20));
    assert_eq!(
        crate::services::query::latest_versions::requested_latest_version_count(&outcome.ir),
        20
    );
}

#[tokio::test]
async fn exact_version_literal_does_not_synthesize_latest_release_slice() {
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["release", "version"],
        "target_entities": [{"label": "1.2.3", "role": "subject"}],
        "literal_constraints": [{"text": "1.2.3", "kind": "version"}],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": {"hint": "release 1.2.3"},
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.95
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "what changed in release 1.2.3", &[])
        .await
        .expect("compile ok");

    assert!(outcome.ir.source_slice.is_none());
    assert!(!crate::services::query::latest_versions::query_requests_latest_versions(&outcome.ir));
}

#[tokio::test]
async fn preserves_focused_typed_latest_release_slice() {
    let ir_json = json!({
        "act": "describe",
        "scope": "single_document",
        "language": "en",
        "target_types": ["release"],
        "target_entities": [{"label": "Alpha Suite", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": {"direction": "tail", "count": 1, "filter": "release_marker"},
        "confidence": 0.9
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "latest Alpha Suite release", &[])
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
    assert_eq!(
        outcome.ir.source_slice.as_ref().map(|slice| slice.direction),
        Some(SourceSliceDirection::Tail)
    );
}

#[tokio::test]
async fn preserves_follow_up_when_history_exists() {
    let ir_json = json!({
        "act": "follow_up",
        "scope": "single_document",
        "language": "en",
        "target_types": ["service"],
        "target_entities": [{"label": "TargetName", "role": "subject"}],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [{"surface": "how", "kind": "bare_interrogative"}],
        "needs_clarification": null,
        "source_slice": null,
        "retrieval_query": "TargetName how",
        "confidence": 0.75
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;
    let binding = sample_binding();
    let history = vec![CompileHistoryTurn {
        role: "assistant".to_string(),
        content: "TargetName was mentioned previously.".to_string(),
    }];

    let outcome = service
        .compile_with_gateway(&gateway, &binding, "how", &history)
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.act, QueryAct::FollowUp);
    assert_eq!(outcome.ir.conversation_refs.len(), 1);
}

#[tokio::test]
async fn returns_provider_failure_on_provider_error() {
    let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let error = service
        .compile_with_gateway(&gateway, &binding, "what is /system/info?", &[])
        .await
        .expect_err("provider failure must fail the compile");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}

#[tokio::test]
async fn returns_provider_failure_on_invalid_ir_output() {
    let gateway = StubGateway::repeating(Ok(chat_response_with("not valid json")));
    let service = QueryCompilerService;
    let binding = sample_binding();

    let error = service
        .compile_with_gateway(&gateway, &binding, "anything", &[])
        .await
        .expect_err("invalid IR must fail the compile");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
}
