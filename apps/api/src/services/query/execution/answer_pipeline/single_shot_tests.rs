use super::*;

#[test]
fn answer_generation_question_prefers_original_user_text() {
    assert_eq!(
        answer_generation_question(
            "scope: prior answer\nentities: alpha-one, alpha-two\nquestion: describe each",
            "describe each",
        ),
        "describe each"
    );
    assert_eq!(answer_generation_question("compiled focus", "  "), "compiled focus");
}

#[test]
fn answer_question_for_disposition_uses_effective_question_unless_blank() {
    assert_eq!(
        answer_question_for_disposition(" compiled focus ", "user question"),
        "compiled focus"
    );
    assert_eq!(answer_question_for_disposition("   ", "user question"), "user question");
}

#[test]
fn no_evidence_candidate_requires_no_prior_answer_generation() {
    assert!(requires_no_evidence_candidate(true, false));
    assert!(!requires_no_evidence_candidate(false, false));
    assert!(!requires_no_evidence_candidate(true, true));
    assert!(!requires_no_evidence_candidate(false, true));
}
#[test]
fn variant_coverage_warning_routes_single_shot_to_agent_path() {
    let query_ir = sample_ir(0.8, None);
    let answer = "Atlas uses the primary adapter, while Boreal uses the secondary module, and both procedures include validation.";
    let generation = super::super::types::AnswerGenerationStage {
        intent_profile: crate::services::query::planner::QueryIntentProfile {
            act: Some(QueryAct::ConfigureHow),
            broad_procedure_variant_coverage: true,
            ..Default::default()
        },
        primary_answer_chunks: Vec::new(),
        authoritative_evidence: super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        assistant_grounding: AssistantGroundingEvidence::default(),
        answer: answer.to_string(),
        usage_json: serde_json::Value::Null,
        prompt_context: answer.to_string(),
        query_ir: query_ir.clone(),
    };
    let accepted_verification = super::super::types::AnswerVerificationStage {
        generation: generation.clone(),
        verification: RuntimeAnswerVerification {
            state: QueryVerificationState::Verified,
            warnings: Vec::new(),
            unsupported_literals: Vec::new(),
        },
    };
    let incomplete_verification = super::super::types::AnswerVerificationStage {
        generation,
        verification: RuntimeAnswerVerification {
            state: QueryVerificationState::PartiallySupported,
            warnings: vec![crate::domains::query::QueryVerificationWarning {
                code: "variant_coverage_incomplete".to_string(),
                message: "Answer does not cover multiple grounded procedure variants.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
            unsupported_literals: Vec::new(),
        },
    };

    assert!(super::single_shot_answer_is_acceptable(
        answer,
        &accepted_verification,
        0,
        &query_ir,
        answer,
    ));
    assert!(!super::single_shot_answer_is_acceptable(
        answer,
        &incomplete_verification,
        0,
        &query_ir,
        answer,
    ));
}

#[test]
fn fast_path_verifier_keeps_selected_runtime_grounding_without_attesting_prose() {
    let prepared = prepared_for_single_shot(sample_ir(0.8, None));

    let chunks = selected_runtime_answer_chunks(&prepared);
    let grounding =
        selected_runtime_grounding_evidence(&prepared, AssistantGroundingEvidence::default());
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which fragment is present?",
        answer: "The selected fragment is context-fragment-a.",
        intent_profile: &prepared.structured.intent_profile,
        evidence: &super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        chunks: &chunks,
        prompt_context: &prepared.answer_context,
        assistant_grounding: &grounding,
    });

    assert!(chunks.is_empty());
    assert!(
        grounding
            .verification_corpus
            .iter()
            .any(|fragment| fragment.contains("context-fragment-a"))
    );
    assert_eq!(verification.state, QueryVerificationState::NotRun);
}

#[test]
fn setup_configuration_builder_verifies_literals_without_attesting_label_prose() {
    let mut prepared = prepared_for_single_shot(sample_ir(0.8, None));
    prepared.query_ir.target_types =
        vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];
    prepared.structured.context_chunks = vec![
        setup_anchor_chunk(
            Uuid::now_v7(),
            "workflow module alpha guide",
            "alpha-agent",
            "/etc/alpha-agent.conf",
        ),
        setup_anchor_chunk(
            Uuid::now_v7(),
            "workflow module beta guide",
            "beta-agent",
            "/etc/beta-agent.conf",
        ),
    ];

    let chunks = selected_runtime_answer_chunks(&prepared);
    let grounding =
        selected_runtime_grounding_evidence(&prepared, AssistantGroundingEvidence::default());
    let answer = super::super::answer::build_setup_configuration_anchor_answer(
        "How do I configure the workflow module?",
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    )
    .expect("setup configuration answer");

    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "How do I configure the workflow module?",
        answer: &answer,
        intent_profile: &prepared.structured.intent_profile,
        evidence: &super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        chunks: &chunks,
        prompt_context: &prepared.answer_context,
        assistant_grounding: &grounding,
    });

    let ru_labels = crate::services::query::i18n::RU_DETERMINISTIC_ANSWER_LABELS;
    assert!(answer.contains(&format!("**{}:**", ru_labels.variants)), "{answer}");
    assert!(!answer.contains("`setup_variants`:"));
    assert!(!answer.contains("`source`:"));
    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"),
        "{:?}",
        verification.warnings
    );
}

#[test]
fn latest_version_enumeration_uses_single_shot_when_context_is_prepared() {
    let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Version];
    ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
        text: "5".to_string(),
        kind: crate::domains::query_ir::LiteralKind::NumericCode,
    });
    let prepared = prepared_for_single_shot(ir);

    assert!(
        super::should_use_single_shot_answer("q", &prepared, None),
        "latest-version retrieval already prepares grounded context and must not force a second retrieval pass"
    );
}

#[test]
fn focused_configuration_inventory_waits_for_evidence_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Concept,
    ];
    ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup installs `alpha-connector`.
Use `/opt/alpha/modules/connector/connector.conf`.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
[UI.ScanPanel.qrCode]
visible = true
"#
    .to_string();

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "focused configuration inventories need evidence preflight so the answer can keep package, file, section, and parameter coverage together"
    );
}

#[test]
fn versioned_update_procedure_waits_for_deterministic_answer_before_single_shot() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
    ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut prepared = prepared_for_single_shot(ir);
    prepared.structured.context_chunks = vec![procedure_chunk(
        "Sample Target runbook",
        "Sample Target update:\n\
             1. Stop Alpha subject workers.\n\
             2. Install alpha-subject-2.0.0.\n\
             3. Start Alpha subject workers.",
    )];

    assert!(
        !super::should_use_single_shot_answer("how to update Sample Target?", &prepared, None,),
        "command-bearing versioned procedures should be answered by the deterministic path instead of the initial single-shot fast path"
    );
}

#[test]
fn update_procedure_builder_uses_selected_runtime_chunks() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
    ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut prepared = prepared_for_single_shot(ir);
    prepared.structured.context_chunks =
        vec![procedure_chunk("Sample Target overview", "Sample Target overview only.")];
    let mut selected_unit = procedure_chunk(
        "Sample Target update",
        "Sample Target update:\n\
             1. stop sample workers\n\
             2. run sample updater\n\
             3. start sample workers",
    );
    selected_unit.chunk_kind = Some(super::super::SOURCE_UNIT_CHUNK_KIND.to_string());
    prepared.structured.ordered_source_units = vec![selected_unit];

    assert!(
        super::super::answer::build_update_procedure_sequence_answer(
            &prepared.query_ir,
            &prepared.structured.context_chunks,
        )
        .is_none()
    );
    let selected_chunks = selected_runtime_answer_chunks(&prepared);
    let answer = super::super::answer::build_update_procedure_sequence_answer(
        &prepared.query_ir,
        &selected_chunks,
    )
    .expect("update procedure answer from selected chunks");

    assert!(answer.contains("run sample updater"), "{answer}");
    assert!(answer.contains("start sample workers"), "{answer}");
    assert!(!answer.contains("overview only"), "{answer}");
}

#[test]
fn update_procedure_builder_keeps_source_context_out_when_primary_evidence_answers() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
    ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some("how to update Sample Target?".to_string());

    let primary = procedure_chunk(
        "Sample Target primary runbook",
        "Sample Target update:\n\
             1. stop sample-primary-worker\n\
             2. run sample-primary-updater\n\
             3. start sample-primary-worker",
    );
    let mut source_context = procedure_chunk(
        "Sample Target source-context companion",
        "Sample Target update:\n\
             1. stop source-context-worker\n\
             2. run source-context-precheck\n\
             3. run source-context-migration\n\
             4. run source-context-validation\n\
             5. start source-context-worker",
    );
    source_context.score_kind = RuntimeChunkScoreKind::SourceContext;

    let (answer, selected_chunks) =
        build_update_procedure_answer_with_source_context_fallback(&ir, &[primary, source_context])
            .expect("primary procedure answer");

    assert!(answer.contains("sample-primary-updater"), "{answer}");
    assert!(!answer.contains("source-context-migration"), "{answer}");
    assert!(
        selected_chunks
            .iter()
            .all(|chunk| chunk.score_kind != RuntimeChunkScoreKind::SourceContext),
        "primary answer verification must not include source-context evidence"
    );
}

#[test]
fn update_procedure_builder_uses_source_context_as_fallback() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
    ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some("how to update Sample Target?".to_string());

    let primary = procedure_chunk(
        "Sample Target overview",
        "Sample Target overview without an ordered procedure.",
    );
    let mut source_context = procedure_chunk(
        "Sample Target source-context runbook",
        "Sample Target update:\n\
             1. stop source-context-worker\n\
             2. run source-context-updater\n\
             3. start source-context-worker",
    );
    source_context.score_kind = RuntimeChunkScoreKind::SourceContext;

    let (answer, selected_chunks) =
        build_update_procedure_answer_with_source_context_fallback(&ir, &[primary, source_context])
            .expect("source-context fallback procedure answer");

    assert!(answer.contains("source-context-updater"), "{answer}");
    assert!(
        selected_chunks
            .iter()
            .any(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext),
        "fallback verification must use the exact evidence set that produced the answer"
    );
}

#[test]
fn low_confidence_structural_configuration_inventory_waits_for_evidence_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types.clear();
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    ir.literal_constraints =
        vec![LiteralSpan { text: "alphaMode".to_string(), kind: LiteralKind::Identifier }];
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup installs `alpha-connector`.
Use `/opt/alpha/modules/connector/connector.conf`.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
[UI.ScanPanel.qrCode]
visible = true
"#
    .to_string();

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "low-confidence untyped compiler output must not bypass normalized configuration coverage when the evidence itself carries a setup inventory"
    );
}

#[test]
fn low_confidence_section_parameter_inventory_waits_for_evidence_preflight_without_path() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types.clear();
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha configuration fragment.
[Main]
endpointUrl
partnerId
secretKey
currency
timeout
visible
"#
    .to_string();

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "section-scoped parameter inventories need evidence preflight even when the file path did not fit in the packed context"
    );
}

#[test]
fn focused_configuration_single_setting_can_use_initial_fast_path() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Concept,
    ];
    ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup uses `/opt/alpha/display/display.ini`.
[UI.ScanPanel.qrCode]
visible = true
"#
    .to_string();

    assert!(
        !super::focused_configuration_inventory_waits_for_preflight(
            &prepared.query_ir,
            &prepared.answer_context,
        ),
        "a focused single-setting fragment is not a broader configuration inventory"
    );
}

#[test]
fn structural_literal_comparison_with_exact_literals_waits_for_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::Compare, 0.93, None);
    ir.retrieval_query =
        Some("Alpha unit and Beta unit SAMPLE_LIMIT threshold controls".to_string());
    ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
        a: Some("Alpha unit".to_string()),
        b: Some("Beta unit".to_string()),
        dimension: "SAMPLE_LIMIT threshold controls".to_string(),
    });
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = r#"
[EVIDENCE_CHUNK document="alpha.txt"] Alpha unit applies SAMPLE_LIMIT_REQUESTS before admitting a record.

[EVIDENCE_CHUNK document="beta.txt"] Beta unit applies SAMPLE_LIMIT_WINDOW_SECONDS before expiring a record.
"#
        .to_string();

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "cross-surface comparisons with exact structural literals need evidence preflight"
    );
}

#[test]
fn disposition_preserves_typed_clarification_with_multiple_retrieved_sources() {
    let mut ir = sample_ir(
        0.74,
        Some(crate::domains::query_ir::ClarificationReason::MultipleInterpretations),
    );
    ir.target_entities =
        vec![EntityMention { label: "return process".to_string(), role: EntityRole::Subject }];
    let mut prepared = prepared_for_single_shot(ir);
    prepared.structured.retrieved_context_document_titles = vec![
        "Return process".to_string(),
        "Return process: attachment.png".to_string(),
        "Adjacent return workflow".to_string(),
    ];
    prepared.structured.retrieved_documents = vec![
        retrieved_doc("Return process", "source://return-process"),
        retrieved_doc("Return process: attachment.png", "source://return-attachment"),
        retrieved_doc("Adjacent return workflow", "source://adjacent-return"),
    ];
    prepared.structured.diagnostics.grouped_references = sample_groups();
    let disposition = classify_answer_disposition(&prepared, "How do I complete return?");

    assert!(
        matches!(disposition, AnswerDisposition::Clarify { .. }),
        "retrieval consolidation must not override typed clarification intent"
    );
}

#[test]
fn stateless_conversation_refs_can_use_initial_fast_path() {
    let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
    ir.conversation_refs.push(UnresolvedRef {
        surface: "source-local anchor".to_string(),
        kind: ConversationRefKind::Deictic,
    });
    let prepared = prepared_for_single_shot(ir);

    assert!(
        super::should_use_single_shot_answer("q", &prepared, None),
        "without prior conversation, unresolved refs cannot be resolved by another retrieval pass and prepared context should answer or refuse directly"
    );
}

#[test]
fn literal_free_retrieve_value_waits_for_source_coverage_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
    ir.target_entities = vec![EntityMention {
        label: "sample service policy".to_string(),
        role: EntityRole::Subject,
    }];
    let prepared = prepared_for_single_shot(ir);

    assert!(
        !super::should_use_single_shot_answer(
            "What renewal policy applies to the sample service?",
            &prepared,
            None,
        ),
        "literal-free value lookups need source coverage before the model decides facts are missing"
    );
}

#[test]
fn conversation_refs_with_history_wait_for_evidence_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
    ir.conversation_refs.push(UnresolvedRef {
        surface: "that topic".to_string(),
        kind: ConversationRefKind::Deictic,
    });
    let prepared = prepared_for_single_shot(ir);

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, Some("user: earlier topic")),
        "real prior conversation should skip only the initial fast path; evidence preflight still answers from fixed evidence"
    );
}

#[test]
fn focused_document_answer_intent_waits_for_evidence_preflight() {
    let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
    ir.target_types = vec![QueryTargetKind::SecondaryHeading];
    let prepared = prepared_for_single_shot(ir);

    assert!(
        !super::should_use_single_shot_answer(
            "What report name appears in the runtime PDF upload check?",
            &prepared,
            None,
        ),
        "focused document literals need primary chunks before final answer selection"
    );
}
