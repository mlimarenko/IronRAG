use super::*;

#[test]
fn broad_procedure_requires_multiple_grounded_variants() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let answer = "1. Install the integration.\n2. Enable the required option.";
    let chunks = vec![
        runtime_chunk_for_document(
            primary_document_id,
            "Sample Connector Atlas setup",
            "Install `atlas-adapter.pkg` and set `atlasMode=true`.",
        ),
        runtime_chunk_for_document(
            secondary_document_id,
            "Sample Connector Boreal setup",
            "Install `boreal-adapter.pkg` and set `borealMode=true`.",
        ),
        runtime_chunk_for_document(Uuid::now_v7(), "Generic setup summary", answer),
    ];
    let verification = verify_broad_procedure(answer, &chunks);

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "variant_coverage_incomplete")
    );
}

#[test]
fn broad_procedure_rejects_document_titles_without_variant_procedure_evidence() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let answer = "For Sample Connector Atlas setup and Sample Connector Boreal setup, install `atlas-adapter.pkg`.";
    let chunks = vec![
        runtime_chunk_for_document(
            primary_document_id,
            "Sample Connector Atlas setup",
            "Install `atlas-adapter.pkg` and set `atlasMode=true`.",
        ),
        runtime_chunk_for_document(
            secondary_document_id,
            "Sample Connector Boreal setup",
            "Install `boreal-adapter.pkg` and set `borealMode=true`.",
        ),
    ];
    let verification = verify_broad_procedure(answer, &chunks);

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "variant_coverage_incomplete")
    );
}

#[test]
fn broad_procedure_rejects_one_shared_anchor_across_documents() {
    let answer = "1. Install the integration before enabling the profile.";
    let chunks = vec![
        runtime_chunk_for_document(
            Uuid::now_v7(),
            "Sample Connector Atlas setup",
            "Install the integration before enabling the profile. Atlas then uses its local flow.",
        ),
        runtime_chunk_for_document(
            Uuid::now_v7(),
            "Sample Connector Boreal setup",
            "Install the integration before enabling the profile. Boreal then uses its remote flow.",
        ),
    ];
    let verification = verify_broad_procedure(answer, &chunks);

    assert!(
        verification.warnings.iter().any(|warning| warning.code == "variant_coverage_incomplete")
    );
}

#[test]
fn broad_procedure_verifies_two_grounded_variant_anchors() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let answer =
        "1. For Atlas, install `atlas-adapter.pkg`.\n2. For Boreal, install `boreal-adapter.pkg`.";
    let chunks = vec![
        runtime_chunk_for_document(
            primary_document_id,
            "Sample Connector Atlas setup",
            "For Atlas, install `atlas-adapter.pkg`.",
        ),
        runtime_chunk_for_document(
            secondary_document_id,
            "Sample Connector Boreal setup",
            "For Boreal, install `boreal-adapter.pkg`.",
        ),
    ];
    let verification = verify_broad_procedure(answer, &chunks);

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification.warnings.iter().all(|warning| warning.code != "variant_coverage_incomplete")
    );
}

#[test]
fn title_only_search_result_is_not_authoritative_verifier_evidence() {
    let answer = "Alpha Guide";
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which guide covers the subject?",
        answer,
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: answer,
        assistant_grounding: &AssistantGroundingEvidence {
            verification_corpus: vec![
                "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\"}]}"
                    .to_string(),
            ],
            document_references: Vec::new(),
        },
    });

    assert_ne!(verification.state, QueryVerificationState::Verified);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "no_\u{63}anonical_evidence")
    );
}

#[test]
fn broad_procedure_recognizes_source_local_prose_for_two_variants() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let answer = "1. For Atlas, install the primary adapter before enabling the local profile.\n\
                  2. For Boreal, register the secondary module before selecting the remote profile.";
    let chunks = vec![
        runtime_chunk_for_document(
            primary_document_id,
            "Sample Connector Atlas setup",
            "For Atlas, install the primary adapter before enabling the local profile.",
        ),
        runtime_chunk_for_document(
            secondary_document_id,
            "Sample Connector Boreal setup",
            "For Boreal, register the secondary module before selecting the remote profile.",
        ),
    ];
    let verification = verify_broad_procedure(answer, &chunks);

    assert!(
        verification.warnings.iter().all(|warning| warning.code != "variant_coverage_incomplete")
    );
}

#[test]
fn verbatim_supported_description_does_not_verify_a_procedure_request() {
    let answer = "The selected component reports a connection failure.";
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Configure Subject Alpha.",
        answer,
        intent_profile: &QueryIntentProfile {
            act: Some(crate::domains::query_ir::QueryAct::ConfigureHow),
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(answer)],
        prompt_context: answer,
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().any(|warning| warning.code == "intent_mismatch"));
}
