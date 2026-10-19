use super::*;

#[test]
fn verify_answer_allows_user_supplied_scope_literals() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which `alpha-*` modules exist?",
        answer: "For `alpha-*`, the grounded context mentions Alpha Sync.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The grounded context mentions Alpha Sync.")],
        prompt_context: "The grounded context mentions Alpha Sync.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::NotRun);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_does_not_ground_non_wildcard_literals_from_question() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Is `/not-grounded` configured?",
        answer: "The configured endpoint is `/not-grounded`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The docs mention a stable endpoint but do not name it.")],
        prompt_context: "The docs mention a stable endpoint but do not name it.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "unsupported_literal"),
        "{:?}",
        verification.warnings
    );
}

#[test]
fn verify_answer_accepts_method_path_literal_when_method_and_path_are_grounded() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which endpoints are needed?",
        answer: "The endpoint is `GET /system/info`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: vec![KnowledgeChunkRow {
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

                occurred_at: None,
                occurred_until: None,
            }],
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        chunks: &[],
        prompt_context: "",
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
fn verify_answer_rejects_method_path_literal_spread_across_fragments() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which endpoints are needed?",
        answer: "The endpoint is `GET /system/info`.",
        intent_profile: &QueryIntentProfile {
            exact_literal_technical: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks: &[
            runtime_chunk("The method GET is used for status requests."),
            runtime_chunk("The path /system/info is mentioned in an unrelated example."),
        ],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["GET /system/info"]);
}

#[test]
fn verify_answer_accepts_literals_grounded_by_runtime_corpus() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "what is the logic in the code",
        answer: "These files show backend logic in Rust. `query_repository.rs` stores `query_conversation`, `query_turn`, and `query_execution`. `audit_repository.rs` filters audit by `action_kind` and writes `iam.bootstrap.claim`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence {
            verification_corpus: vec![
                r#"{"structuredContent":{"documentTitle":"query_repository.rs","content":"from query_conversation and query_turn joined to query_execution"},"isError":false}"#
                    .to_string(),
                r#"{"structuredContent":{"documentTitle":"audit_repository.rs","content":"append_audit_event filters by action_kind and writes iam.bootstrap.claim"},"isError":false}"#
                    .to_string(),
            ],
            document_references: Vec::new(),
        },
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_source_uri_literals_without_source_excerpt_support() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which source was cited?",
        answer: "Source: `https://example.test/docs/alpha`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence {
            verification_corpus: Vec::new(),
            document_references: vec![
                crate::services::query::assistant_grounding::AssistantGroundingDocumentReference {
                    document_id: Uuid::now_v7(),
                    revision_id: Some(Uuid::now_v7()),
                    document_title: "Alpha Guide".to_string(),
                    source_uri: Some("https://example.test/docs/alpha".to_string()),
                    source_access: None,
                    slice_start_offset: 0,
                    slice_end_offset: 24,
                    excerpt: "The Alpha Guide is the cited source.".to_string(),
                    rank: 1,
                },
            ],
        },
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(verification.warnings.iter().any(|warning| warning.code == "unsupported_literal"));
}

#[test]
fn verify_answer_records_unsupported_literals_for_revision_guard() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which command is required?",
        answer: "Run `democtl missing --flag`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Run democtl present --flag.")],
        prompt_context: "Run democtl present --flag.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["democtl missing --flag"]);
}
