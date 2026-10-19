use super::*;

/// Verify an answer to the canonical port question against one runtime chunk
/// that also serves as the prompt context.
fn verify_exact_port_answer(answer: &str, evidence_chunk: &str) -> RuntimeAnswerVerification {
    verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which port does the service use?",
        answer,
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(evidence_chunk)],
        prompt_context: evidence_chunk,
        assistant_grounding: &AssistantGroundingEvidence::default(),
    })
}

#[test]
fn verify_answer_rejects_unformatted_exact_numeric_claim() {
    let verification = verify_exact_port_answer(
        "The service listens on port 9090.",
        "The service listens on port 8080.",
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["9090"]);
}

#[test]
fn verify_answer_does_not_treat_order_marker_as_verified_content() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "How should the service be restarted?",
        answer: "15. Restart the service and verify its status.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Restart the service and verify its status.")],
        prompt_context: "Restart the service and verify its status.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_exact_answer_fails_closed_for_unchecked_numbered_steps() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "How should the adapter be prepared?",
        answer: "1. Configure the adapter.\n2. Verify its status.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The adapter setup procedure covers connection and status validation.",
        )],
        prompt_context: "The adapter setup procedure covers connection and status validation.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
    assert!(
        verification
            .warnings
            .iter()
            .all(|warning| warning.code != "unsupported_\u{63}anonical_claim")
    );
}

#[test]
fn verify_answer_does_not_support_exact_number_from_larger_number() {
    let verification = verify_exact_port_answer(
        "The service listens on port 9090.",
        "The unrelated record number is 19090.",
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["9090"]);
}

#[test]
fn verify_answer_rejects_unformatted_exact_number_before_sentence_punctuation() {
    let verification = verify_exact_port_answer(
        "The service listens on port 9090!",
        "The service listens on port 8080.",
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["9090"]);
}

#[test]
fn verify_answer_rejects_unformatted_exact_number_after_assignment_separator() {
    let verification =
        verify_exact_port_answer("The service uses port=9090.", "The service uses port=8080.");

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["9090"]);
}

#[test]
fn verify_exact_answer_does_not_verify_unchecked_identifier_suffix() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which epoch is recorded?",
        answer: "The recorded epoch is 2029λ.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The recorded epoch is 2030λ.")],
        prompt_context: "The recorded epoch is 2030λ.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_answer_does_not_verify_unchecked_multi_letter_identifier_suffix() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which record is selected?",
        answer: "The selected record is 2029λx.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The selected record is 2030λx.")],
        prompt_context: "The selected record is 2030λx.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_answer_state_does_not_depend_on_answer_prose() {
    let answer =
        "The record quotes `no grounded evidence` and `exact value is not grounded` as labels.";
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which labels does the record quote?",
        answer,
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(answer)],
        prompt_context: answer,
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_does_not_support_positive_number_from_negative_evidence() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "What is the exact configured temperature?",
        answer: "The configured temperature is 90.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The configured temperature is -90.")],
        prompt_context: "The configured temperature is -90.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["90"]);
}

#[test]
fn verify_answer_accepts_supported_unformatted_exact_numeric_claim() {
    let verification = verify_exact_port_answer(
        "The service listens on port 8080.",
        "The service listens on port 8080.",
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_answer_marks_unchecked_non_exact_prose_as_not_run() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Summarize the procedure.",
        answer: "The response organizes the procedure into 3 concise steps.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The procedure contains preparation, execution, and validation.")],
        prompt_context: "The procedure contains preparation, execution, and validation.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_answer_does_not_verify_unchecked_year_for_non_exact_intent() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "When is the policy effective?",
        answer: "The policy becomes effective in 2031.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The policy becomes effective in 2029.")],
        prompt_context: "The policy becomes effective in 2029.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| { warning.code == "semantic_verification_not_run" })
    );
}

#[test]
fn verify_answer_does_not_verify_unchecked_version_and_date_for_non_exact_intent() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Summarize the selected release record.",
        answer: "Release v9.9.9 was published on 2031-05-06.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Release v1.2.3 was published on 2029-04-05.")],
        prompt_context: "Release v1.2.3 was published on 2029-04-05.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.unsupported_literals.is_empty());
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| { warning.code == "semantic_verification_not_run" })
    );
}

#[test]
fn verify_exact_answer_accepts_supported_date_and_version_in_ordinary_prose() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "When was the release published?",
        answer: "Release v2.4.1 was published on 2029-04-05.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Release v2.4.1 was published on 2029-04-05.")],
        prompt_context: "Release v2.4.1 was published on 2029-04-05.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.unsupported_literals.is_empty());
}

#[test]
fn verify_exact_answer_rejects_unsupported_date_in_ordinary_prose() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "When was the selected record published?",
        answer: "The selected record was published on 2031-05-06.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The selected record was published on 2029-04-05.")],
        prompt_context: "The selected record was published on 2029-04-05.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["2031-05-06"]);
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| warning.code == "unsupported_\u{63}anonical_claim")
    );
}

#[test]
fn verify_answer_does_not_support_year_from_larger_identifier_number() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which year applies?",
        answer: "The applicable year is 2029.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The unrelated record identifier is ID 12029.")],
        prompt_context: "The unrelated record identifier is ID 12029.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["2029"]);
}

#[test]
fn verify_answer_does_not_support_version_from_longer_version_prefix() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which version applies?",
        answer: "The applicable version is v2.4.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The selected source documents v2.40.")],
        prompt_context: "The selected source documents v2.40.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["v2.4"]);
}

#[test]
fn verify_exact_answer_marks_unchecked_named_prose_as_partial() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Who owns the endpoint?",
        answer: "The service owner is Morgan and uses endpoint `/status`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The service owner is Alex and uses endpoint /status.")],
        prompt_context: "The service owner is Alex and uses endpoint /status.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.unsupported_literals.is_empty());
    assert!(
        verification
            .warnings
            .iter()
            .any(|warning| { warning.code == "semantic_verification_partial" })
    );
}

#[test]
fn verify_exact_answer_still_checks_explicitly_marked_named_literals() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which owner is recorded?",
        answer: "The recorded owner is `ExampleOwner`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The selected record identifies a different owner.")],
        prompt_context: "The selected record identifies a different owner.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["ExampleOwner"]);
    assert!(verification.warnings.iter().any(|warning| warning.code == "unsupported_literal"));
}
