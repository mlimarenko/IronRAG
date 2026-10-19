use super::*;

#[test]
fn verify_answer_marks_conflicting_even_when_one_literal_is_grounded() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Use the exact WSDL URL.",
        answer: "Use `http://demo.local:8080/customer-profile/ws/customer-profile.wsdl`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    normalized_value_text: "http://demo.local:8080/customer-profile/ws/"
                        .to_string(),
                    normalized_value_exact: "http://demo.local:8080/customer-profile/ws/"
                        .to_string(),
                    normalized_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/"
                    ),
                    display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    normalized_value_text:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    normalized_value_exact:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    normalized_value_json: serde_json::json!(
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
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(
        verification.state,
        QueryVerificationState::Conflicting,
        "unexpected verification result: {verification:?}"
    );
    assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
}

#[test]
fn verify_answer_reports_typed_conflict_without_inventing_named_claims() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("endpoint:{}", Uuid::now_v7());
    let technical_facts = ["/system/info", "/system/status"]
        .into_iter()
        .map(|value| KnowledgeTechnicalFactRow {
            normalized_value_text: value.to_string(),
            normalized_value_exact: value.to_string(),
            normalized_value_json: serde_json::json!(value),
            display_value: value.to_string(),
            conflict_group_id: Some(conflict_group_id.clone()),
            fact_kind: "endpoint_path".to_string(),
            ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
        })
        .collect();
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which exact endpoint should be used?",
        answer: "Use Acme endpoint `/system/info`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts,
        },
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Conflicting);
    assert!(verification.unsupported_literals.is_empty());
    assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
}

#[test]
fn verify_answer_marks_conflicting_when_exact_literal_question_stays_ambiguous() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "What exact endpoint is described?",
        answer: "The exact endpoint is described in the selected evidence.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    normalized_value_text: "/system/info".to_string(),
                    normalized_value_exact: "/system/info".to_string(),
                    normalized_value_json: serde_json::json!("/system/info"),
                    display_value: "/system/info".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    normalized_value_text: "/system/status".to_string(),
                    normalized_value_exact: "/system/status".to_string(),
                    normalized_value_json: serde_json::json!("/system/status"),
                    display_value: "/system/status".to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Conflicting);
    assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
}
