use super::*;

#[test]
fn verify_answer_accepts_structural_config_literal_from_an_exact_source_line() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which feature flag is enabled?",
        answer: "Set `[AlphaFeature] enableFlag = true`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "Configuration section:\n[AlphaFeature] enableFlag = true\nEnd of section.",
        )],
        prompt_context: "Configuration section:\n[AlphaFeature] enableFlag = true\nEnd of section.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_structural_config_literals_spread_across_fragments() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which feature flag is enabled?",
        answer: "Set `[AlphaFeature] enableFlag = true`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[
            runtime_chunk("Configuration section [AlphaFeature] contains enableFlag."),
            runtime_chunk("Another section documents true for a different key."),
        ],
        prompt_context: "Configuration section [AlphaFeature] contains enableFlag. Another section documents true for a different key.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["[AlphaFeature] enableFlag = true"]);
}

#[test]
fn verify_answer_accepts_code_like_assignment_literal_from_an_exact_source_line() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which assignment is documented?",
        answer: "Use `sendReceiptCopy = false`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Receipt configuration:\nsendReceiptCopy = false")],
        prompt_context: "Receipt configuration:\nsendReceiptCopy = false",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_fully_verifies_an_exact_assignment_answer() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which assignment is documented?",
        answer: "`sendReceiptCopy = false`",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("sendReceiptCopy = false")],
        prompt_context: "sendReceiptCopy = false",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_accepts_exact_assignment_from_one_structured_fact() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let assignment = "sendReceiptCopy = false";
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which assignment is documented?",
        answer: "`sendReceiptCopy = false`",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                normalized_value_text: assignment.to_string(),
                normalized_value_exact: assignment.to_string(),
                normalized_value_json: serde_json::json!(assignment),
                display_value: assignment.to_string(),
                fact_kind: "configuration_key".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_rejects_assignment_components_separated_within_one_fragment() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which assignment is documented?",
        answer: "Use `sendReceiptCopy = false`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(&format!(
            "sendReceiptCopy controls output. {} false belongs to another setting.",
            "neutral filler ".repeat(64)
        ))],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["sendReceiptCopy = false"]);
}

#[test]
fn verify_answer_rejects_assignment_missing_its_bracket_qualifier() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which qualified assignment is documented?",
        answer: "Use `[PROD] APP_DATABASE_URL = postgres://db.example/app`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("APP_DATABASE_URL = postgres://db.example/app")],
        prompt_context: "APP_DATABASE_URL = postgres://db.example/app",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(
        verification.unsupported_literals,
        vec!["[PROD] APP_DATABASE_URL = postgres://db.example/app"]
    );
}

#[test]
fn verify_answer_rejects_angle_url_as_placeholder() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which command is documented?",
        answer: "Run `toolctl <https://example.invalid/path>`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The toolctl command accepts a target argument.")],
        prompt_context: "The toolctl command accepts a target argument.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["toolctl <https://example.invalid/path>"]);
}

#[test]
fn verify_answer_accepts_shared_prefix_slash_alternatives_from_same_fragment() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which connection keys are documented?",
        answer: "Use `messages.alpha.connection.timeout.user/password`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "Documented keys include messages.alpha.connection.timeout.user and messages.alpha.connection.timeout.password.",
        )],
        prompt_context: "Documented keys include messages.alpha.connection.timeout.user and messages.alpha.connection.timeout.password.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_slash_separated_code_value_lists_from_same_fragment() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which commands and statuses are documented?",
        answer: "Supported commands are `start / stop / restart / status`; status values are `OPEN / CLOSED / ALL`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The service commands are start, stop, restart, and status. Export status values are OPEN, CLOSED, and ALL.",
        )],
        prompt_context: "The service commands are start, stop, restart, and status. Export status values are OPEN, CLOSED, and ALL.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_slash_separated_list_when_an_alternative_is_missing() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which commands are documented?",
        answer: "Supported commands are `start / stop / erase`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The service commands are start and stop.")],
        prompt_context: "The service commands are start and stop.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["start / stop / erase"]);
}

#[test]
fn verify_answer_accepts_decorated_version_literal_when_numeric_version_is_supported() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which release is supported?",
        answer: "The supported build is `Release 7.8.9`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The release note identifies 7.8.9 as the supported build for this feature.",
        )],
        prompt_context: "The release note identifies 7.8.9 as the supported build for this feature.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_decorated_version_literal_when_numeric_version_is_missing() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which release is supported?",
        answer: "The supported build is `Release 9.9.9`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The release note identifies 7.8.9 as the supported build for this feature.",
        )],
        prompt_context: "The release note identifies 7.8.9 as the supported build for this feature.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["Release 9.9.9"]);
}

#[test]
fn verify_answer_rejects_decorated_version_when_only_the_number_is_grounded() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which release is supported?",
        answer: "The supported build is `InventedName 7.8.9`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("The release note identifies 7.8.9 as the supported build.")],
        prompt_context: "The release note identifies 7.8.9 as the supported build.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["InventedName 7.8.9"]);
}

#[test]
fn verify_answer_accepts_structural_command_placeholders_when_command_is_supported() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which command form is documented?",
        answer: "Run `toolctl <TARGET> [MODE]`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The toolctl command accepts a target and an optional mode argument.",
        )],
        prompt_context: "The toolctl command accepts a target and an optional mode argument.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_simple_assignment_without_exact_support() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which status is configured?",
        answer: "Use `status = 1`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "The status field can be CLOSED. Identifier 1 appears in a separate example.",
        )],
        prompt_context: "The status field can be CLOSED. Identifier 1 appears in a separate example.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["status = 1"]);
}

#[test]
fn verify_answer_accepts_quoted_literals_grounded_by_decoded_read_document_content() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            r#"surface_kind = "bootstrap" and result_kind = "succeeded""#.to_string(),
        ],
        document_references: Vec::new(),
    };

    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which filters and events does audit_repository.rs handle?",
        answer: "The file filters by `\"bootstrap\"` and `\"succeeded\"` in the literal-value examples.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &grounding,
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_markdown_escaped_path_literals() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![r#"share path: \\ host\_name \scan-share"#.to_string()],
        document_references: Vec::new(),
    };

    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which path is used?",
        answer: r#"Use `\\host_name\scan-share`."#,
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[],
        prompt_context: "",
        assistant_grounding: &grounding,
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_literals_grounded_by_html_entity_equivalent_context() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which example query was provided?",
        answer: "The example was:\n\n```sql\nSELECT name, age\nFROM students\nWHERE age > 18\nORDER BY name ASC;\n```",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(
            "Example:\nSELECT name, age\nFROM students\nWHERE age &gt; 18\nORDER BY name ASC;",
        )],
        prompt_context: "Example:\nSELECT name, age\nFROM students\nWHERE age &gt; 18\nORDER BY name ASC;",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_accepts_named_decimal_and_hex_html_entities() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which literals were provided?",
        answer: r#"Use `"alpha"`, `beta's`, and `/v1/items`."#,
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Use &quot;alpha&quot;, beta&#39;s, and &#x2F;v1&#x2F;items.")],
        prompt_context: "Use &quot;alpha&quot;, beta&#39;s, and &#x2F;v1&#x2F;items.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"));
}

#[test]
fn verify_answer_rejects_structural_config_literals_spread_too_far_apart() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which feature flag is enabled?",
        answer: "Set `[AlphaFeature] enableFlag = true`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk(&format!(
            "Configuration section [AlphaFeature] contains enableFlag. {} The value true belongs to a separate note.",
            "filler ".repeat(500)
        ))],
        prompt_context: "",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["[AlphaFeature] enableFlag = true"]);
}

#[test]
fn verify_answer_does_not_decode_malformed_html_entity_without_semicolon() {
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which literal was provided?",
        answer: "Use `AT&T`.",
        intent_profile: &QueryIntentProfile::default(),
        evidence: &empty_authoritative_evidence(),
        chunks: &[runtime_chunk("Use AT&ampT.")],
        prompt_context: "Use AT&ampT.",
        assistant_grounding: &AssistantGroundingEvidence::default(),
    });

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert_eq!(verification.unsupported_literals, vec!["AT&T"]);
}
