use super::*;

#[test]
fn verify_answer_accepts_semantic_web_and_knowledge_graph_targets() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        answer: "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: 0,
            chunk_kind: Some("paragraph".to_string()),
            document_label: "concepts.md".to_string(),
            excerpt: "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.".to_string(),
        }],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_does_not_verify_unchecked_prose_from_unrelated_evidence() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which statement is supported?",
        answer: "The selected component performs an unrelated operation.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The selected record documents a different component.")],
        prompt_context: "The selected record documents a different component.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| { warning.code == "semantic_verification_not_run" })
    );
}

#[test]
fn verify_answer_only_partially_supports_literal_when_surrounding_prose_is_unchecked() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which endpoint is documented?",
        answer: "An unsupported owner controls endpoint `/status`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The documented endpoint is /status.")],
        prompt_context: "The documented endpoint is /status.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| { warning.code == "semantic_verification_partial" })
    );
}

#[test]
fn verify_answer_rejects_nonempty_answer_without_authoritative_evidence() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "What is the configured endpoint?",
        answer: "The endpoint is `/not-grounded`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "no_\u{63}anonical_evidence")
    );
}
